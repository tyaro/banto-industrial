// banto v3.0.0 の admin-template
// `apps/admin-template/src/routes/(app)/+layout.ts` を写した（v3 移行）。
// chronogazer 固有の差: grant を使わない（閲覧公開 `publicViewer` も試運転も無い）
// ので、確定した `none` は `grantFallback` を経ずにそのまま /login
// （公開閲覧のナビ制限も無い）。
// エラー画面の本文は i18n ではなく `SESSION_CHECK_FAILED_MESSAGE`（日本語）。
// ロケールの同期（`syncLocaleFromProvider`）は無い。`base` は使わない。
import { error, redirect } from '@sveltejs/kit';
import { getSessionController, resolveSettled } from '@banto/admin-core';
import { bantoReady } from '$lib/banto/setup';
import { SESSION_CHECK_FAILED_MESSAGE } from '$lib/banto/sessionGuard';
import { settings } from '$lib/settings.svelte';

// Auth guard for the whole (app) group (spec §8.1), banto Issue #260 (design
// §6.1, v2.0.0): the session is confirmed by the SessionController - the
// only writer of "who is signed in" (ADR-0016). This load's only side
// effect is the controller's confirmation; it writes no store
// (`sessionStore` is derived from `controller.snapshot`). Must wait for
// provider selection/detection (spec §11.1's three-way environment probe)
// first.
//
// - `resolveSettled()` asks again after `superseded` and returns only
//   `confirmed` or `unverified` (I-16). `unverified` - the server could not
//   verify the session (banto #204: a 500 / unreachable, Tauri's
//   `auth_resolve` hitting a DB error), or it kept changing, or the 10 s
//   deadline passed - is the retryable error page (`routes/+error.svelte`):
//   nothing is cleared, the stored token (Remember me included) is kept, and
//   "再試行" re-runs this load (S-36/S-60: after a switch of user it is NOT
//   left automatically).
// - A CONFIRMED `none` goes to /login. ChronoGazer issues no grant (no
//   public viewing, no commissioning mode), so there is no `grantFallback`
//   step.
export async function load() {
	await bantoReady;
	const controller = getSessionController();
	const result = await resolveSettled(controller, { cause: 'navigation' });
	if (result.outcome === 'unverified') {
		error(503, { message: SESSION_CHECK_FAILED_MESSAGE });
	}
	if (result.snapshot.status !== 'active') redirect(307, '/login');

	// M12: now that the session is confirmed, pull theme settings from the
	// UiSettingsProvider (settings DB) - a value saved from another
	// client/session beats this tab's localStorage cache. Fire-and-forget:
	// navigation must not wait on (or fail with) a settings read.
	void settings.syncFromProvider();

	// The generation THIS load confirmed (I-16) - never a `snapshot.generation`
	// read now: after the awaits above another session may already have been
	// confirmed, and this load's data belongs to the earlier one.
	// `+layout.svelte` renders the page only while it is still the live
	// generation (the generation gate) and re-runs the loads when it is not
	// (wiring ①).
	return { sessionGeneration: result.snapshot.generation };
}
