import { redirect } from '@sveltejs/kit';
import { getAuthProvider } from '@banto/admin-core';
import { bantoReady } from '$lib/banto/setup';
import { sessionStore } from '$lib/session.svelte';
import { settings } from '$lib/settings.svelte';

// Auth guard for the whole (app) group (spec §8.1), backed by
// AuthProvider.check() (spec §3.3). Must wait for provider
// selection/detection (spec §11.1's three-way environment probe) to finish
// before getAuthProvider() is safe to call.
//
// M10 RBAC: also populates `sessionStore` (identity + role) here, right
// after the session is confirmed valid, so every page/component under (app)
// can read `sessionStore.role` synchronously - see session.svelte.ts's doc
// comment for the ordering guarantee this relies on.
export async function load() {
	await bantoReady;
	if (!(await getAuthProvider().check())) {
		redirect(307, '/login');
	}
	await sessionStore.load();

	// M12: now that the session is confirmed, pull theme settings from the
	// UiSettingsProvider (settings DB) - a value saved from another
	// client/session beats this tab's localStorage cache. Fire-and-forget:
	// navigation must not wait on (or fail with) a settings read.
	void settings.syncFromProvider();
}
