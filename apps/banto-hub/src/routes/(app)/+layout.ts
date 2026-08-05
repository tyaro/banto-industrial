import { redirect } from '@sveltejs/kit';
import { getAuthProvider } from '@banto/admin-core';
import { bantoReady } from '$lib/banto/setup';
import { sessionStore } from '$lib/session.svelte';
import { settings } from '$lib/settings.svelte';

// relay-wright の同名ファイルから複製。(app) グループ全体の認証ガード
// （AuthProvider.check() ベース）+ sessionStore（identity/role）の初期化。
export async function load() {
	await bantoReady;
	if (!(await getAuthProvider().check())) {
		redirect(307, '/login');
	}
	await sessionStore.load();

	// セッション確定後に UiSettingsProvider から設定を読み直す
	// （他クライアントで保存された値がこのタブの localStorage キャッシュに
	// 優先する）。fire-and-forget: ナビゲーションを待たせない/失敗させない。
	void settings.syncFromProvider();
}
