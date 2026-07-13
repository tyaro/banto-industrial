import { redirect } from '@sveltejs/kit';
import { isAdmin } from '$lib/permissions';
import { sessionStore } from '$lib/session.svelte';

/**
 * `admin`-only page (spec M10 RBAC): non-admins are sent to the dashboard
 * rather than shown a 403 screen - "hidden by navigation", same philosophy
 * as the sidebar not listing this entry for them (spec §11.3's capability
 * judgement, extended to roles).
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
		redirect(307, '/dashboard');
	}
}
