import { error, redirect } from '@sveltejs/kit';
import { getAuthProvider } from '@banto/admin-core';
import { bantoReady } from '$lib/banto/setup';
import { decideProtectedRoute, SESSION_CHECK_FAILED_MESSAGE } from '$lib/banto/sessionGuard';
import { sessionStore } from '$lib/session.svelte';
import { settings } from '$lib/settings.svelte';

// Auth guard for the whole (app) group (spec §8.1), backed by
// AuthProvider.check() (spec §3.3). Must wait for provider
// selection/detection (spec §11.1's three-way environment probe) to finish
// before getAuthProvider() is safe to call.
//
// banto v1.7.0 #204: `decideProtectedRoute` (`resolveProtectedSession`) only
// sends the user to the login screen when the session is CONFIRMED invalid.
// When it could not be verified (the server's account check failed with a
// 500, the server is unreachable, or Tauri's `auth_check` hit a DB error) the
// guard stops with an error page that offers a retry (`routes/+error.svelte`)
// - the stored token (Remember me included) is kept, and the session resumes
// once the server answers.
//
// M10 RBAC: also populates `sessionStore` (identity + role) here, right
// after the session is confirmed valid, so every page/component under (app)
// can read `sessionStore.role` synchronously - see session.svelte.ts's doc
// comment for the ordering guarantee this relies on.
export async function load() {
	await bantoReady;
	const decision = await decideProtectedRoute(getAuthProvider());
	if (decision === 'unverified') {
		error(503, { message: SESSION_CHECK_FAILED_MESSAGE });
	}
	if (decision === 'login') {
		redirect(307, '/login');
	}
	await sessionStore.load();

	// M12: now that the session is confirmed, pull theme settings from the
	// UiSettingsProvider (settings DB) - a value saved from another
	// client/session beats this tab's localStorage cache. Fire-and-forget:
	// navigation must not wait on (or fail with) a settings read.
	void settings.syncFromProvider();
}
