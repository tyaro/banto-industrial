import { redirect } from '@sveltejs/kit';
import { resolveAppPath } from '#lib/navigation.js';
import { isAdmin } from '#lib/permissions.js';
import { sessionStore } from '#lib/session.svelte.js';

// `routes/(app)/audit-log/+page.ts` から複製（T2-4、admin 限定）。
export async function load({ parent }) {
	await parent();
	if (!isAdmin(sessionStore.role)) {
		redirect(307, resolveAppPath('/status'));
	}
}
