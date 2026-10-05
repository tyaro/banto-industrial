import { redirect } from '@sveltejs/kit';
import { resolveAppPath } from '#lib/navigation.js';
import { isAdmin } from '#lib/permissions.js';
import { sessionStore } from '#lib/session.svelte.js';

// chronogazer の同名ファイルから複製。差分はリダイレクト先のみ
// （chronogazer の /monitor 相当が banto-hub では /status）。
export async function load({ parent }) {
	await parent();
	if (!isAdmin(sessionStore.role)) {
		redirect(307, resolveAppPath('/status'));
	}
}
