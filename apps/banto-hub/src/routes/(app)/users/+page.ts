import { redirect } from '@sveltejs/kit';
import { isAdmin } from '$lib/permissions';
import { sessionStore } from '$lib/session.svelte';

// chronogazer の同名ファイルから複製。差分はリダイレクト先のみ
// （chronogazer の /monitor 相当が banto-hub では /status）。
export async function load({ parent }) {
	await parent();
	if (!isAdmin(sessionStore.role)) {
		redirect(307, '/status');
	}
}
