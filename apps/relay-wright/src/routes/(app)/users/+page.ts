import { redirect } from '@sveltejs/kit';
import { isAdmin } from '$lib/permissions';
import { sessionStore } from '$lib/session.svelte';

/**
 * `admin`-only page (RBAC): non-admins are sent to `/settings` (this app's
 * default landing page - see `routes/+page.ts`) rather than shown a 403
 * screen - "hidden by navigation", same philosophy as the sidebar not
 * listing this entry for them.
 *
 * `await parent()` (rather than reading `sessionStore.role` directly) is
 * required here: SvelteKit does not wait for an ancestor layout's load() to
 * finish before running this one unless asked to, and `(app)/+layout.ts` is
 * what actually populates `sessionStore` - see that file and
 * `session.svelte.ts`'s doc comments.
 */
export async function load({ parent }) {
	await parent();
	if (!isAdmin(sessionStore.role)) {
		redirect(307, '/settings');
	}
}
