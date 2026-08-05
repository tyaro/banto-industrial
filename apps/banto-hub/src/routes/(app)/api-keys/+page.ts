import { redirect } from '@sveltejs/kit';
import { isAdmin } from '$lib/permissions';
import { sessionStore } from '$lib/session.svelte';

// `admin` 限定（users/audit-log と同じ「非adminは状態画面へリダイレクト」
// 方針 - ナビゲーション上も隠れているので404/403画面は出さない）。
export async function load({ parent }) {
	await parent();
	if (!isAdmin(sessionStore.role)) {
		redirect(307, '/status');
	}
}
