import { redirect } from '@sveltejs/kit';
import { isAdmin } from '$lib/permissions';
import { sessionStore } from '$lib/session.svelte';

/**
 * `admin`-only page (spec M14): non-admins are sent to the dashboard rather
 * than shown a 403 screen - same "hidden by navigation" philosophy as
 * `routes/(app)/users/+page.ts` (spec M10), which this mirrors exactly.
 *
 * `await parent()` is required here for the same reason as the users page:
 * SvelteKit does not wait for an ancestor layout's `load()` to finish before
 * running this one unless asked to, and `(app)/+layout.ts` is what actually
 * populates `sessionStore`.
 */
export async function load({ parent }) {
	await parent();
	if (!isAdmin(sessionStore.role)) {
		redirect(307, '/dashboard');
	}
}
