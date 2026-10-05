// banto v3.0.0 の admin-template
// `apps/admin-template/src/routes/(app)/+layout.ts` を写した（v3.0.1 の #321 で
// 起動待ちの延期に差し替え）（v3 移行、I2b で
// 閲覧公開の `grantFallback` を戻した - 2026-10-04 オーナー決定で ChronoGazer
// でも閲覧公開を使う）。
// chronogazer 固有の差: エラー画面の本文は i18n ではなく
// `SESSION_CHECK_FAILED_MESSAGE`（日本語）。ロケールの同期
// （`syncLocaleFromProvider`）は無い。`base` は使わない（ChronoGazer は
// ルート直下に配信する）。
import { error, redirect } from '@sveltejs/kit';
import {
	getAuthProvider,
	getSessionController,
	grantFallback,
	resolveSettled
} from '@banto/admin-core';
import { isBantoReady } from '#lib/banto/setup.js';
import { deferUntilStarted } from '#lib/banto/startupGate.js';
import { SESSION_CHECK_FAILED_MESSAGE } from '#lib/banto/sessionGuard.js';

// 延期の 503 の本文（エラー画面は出さずスプラッシュが出るので実際には見えない）。
const STARTING_MESSAGE = '起動中…';
import { settings } from '#lib/settings.svelte.js';
import { publicNavItems } from '#lib/navigation.js';

// Auth guard for the whole (app) group (spec §8.1), banto Issue #260 (design
// §6.1, v2.0.0): the session is confirmed by the SessionController - the
// only writer of "who is signed in" (ADR-0016). This load's only side
// effects are the controller's confirmation and, for a confirmed `none`,
// the grant policy's issuance (ADR-0017); it writes no store (`sessionStore`
// is derived from `controller.snapshot`). Runs only after provider
// selection/detection (spec §11.1's three-way environment probe) has
// finished - but never WAITS for it (banto #321): while startup is still
// running the guard throws the startup deferral (`startupGate.ts`) before
// touching any provider or the session, so the root layout can show the
// startup splash ("起動中…" -> "サーバーに接続できません" + 再接続, #286) and
// re-run this load once startup has finished. Awaiting `bantoReady` here
// kept the first load - and with it the whole screen - blank while the
// server was unreachable. The child loads all `await parent()`, so none of
// them (and no protected component) runs before this guard has passed.
//
// - `resolveSettled()` asks again after `superseded` and returns only
//   `confirmed` or `unverified` (I-16). `unverified` - the server could not
//   verify the session (banto #204: a 500 / unreachable, Tauri's
//   `auth_resolve` hitting a DB error), or it kept changing, or the 10 s
//   deadline passed - is the retryable error page (`routes/+error.svelte`):
//   nothing is cleared, the stored token (Remember me included) is kept, and
//   "再試行" re-runs this load (S-36/S-60: after a switch of user it is NOT
//   left automatically).
// - viewer-public-plan §3.1-6 (ADR-0012): a CONFIRMED `none` goes through
//   `grantFallback(..., { kind: 'publicViewer' })` - when `server.viewerPublic`
//   is ON (`status()`'s `grants.publicViewer`) it enters the `publicViewer`
//   grant (`enterGrant('publicViewer')`): the fixed-identity
//   `{id:'public',role:'viewer'}` session bound to this confirmation's ticket
//   (S-42/S-52) and confirms it. Its result is handled the same way:
//   `unverified` is the error page, not /login (S-66); only a confirmed
//   `none` goes to /login. Only the HTTP provider implements `status()`'s
//   `grants` and `enterGrant()`; Tauri/demo go straight to /login.
export async function load({ url }) {
	deferUntilStarted(isBantoReady(), () => STARTING_MESSAGE);
	const controller = getSessionController();
	let result = await resolveSettled(controller, { cause: 'navigation' });
	if (result.outcome === 'unverified') sessionCheckFailed();
	if (result.snapshot.status === 'none') {
		result = await grantFallback(controller, getAuthProvider(), result.ticket, {
			kind: 'publicViewer'
		});
		if (result.outcome === 'unverified') sessionCheckFailed();
		if (result.snapshot.status !== 'active') redirect(307, '/login');
	}
	const snapshot = result.snapshot;

	// viewer-public-plan §3.1-6: a public-viewer session may only browse the
	// nav allowlist (`navigation.ts`'s `NavItem.publicViewer`) - RBAC's
	// `viewer` role remains the real data-access boundary (ADR-0012 §帰結),
	// this only keeps the SCREEN a bookmarked/typed URL lands on inside the
	// allowed area, same intent as `users/+page.ts`'s own role redirect but
	// applied to every path under (app) at once.
	if (snapshot.kind === 'publicViewer') {
		const pathname = url.pathname;
		const allowed = publicNavItems().some(
			(item) => pathname === item.path || pathname.startsWith(item.path + '/')
		);
		if (!allowed) {
			const firstPublicNavItem = publicNavItems()[0];
			if (firstPublicNavItem) redirect(307, firstPublicNavItem.path);
		}
	}

	// M12: now that the session is confirmed, pull theme settings from the
	// UiSettingsProvider (settings DB) - a value saved from another
	// client/session beats this tab's localStorage cache. Fire-and-forget:
	// navigation must not wait on (or fail with) a settings read.
	void settings.syncFromProvider();

	// The generation THIS load confirmed (I-16) - never a `snapshot.generation`
	// read now: after the awaits above another session may already have been
	// confirmed (e.g. a login in another tab during the settings sync), and
	// `+layout.svelte` renders the page only while it is still the live
	// generation (the generation gate) and re-runs the loads when it is not
	// (wiring ①).
	return { sessionGeneration: snapshot.generation };
}

/** The retryable error page (banto #204): the session could not be verified. */
function sessionCheckFailed(): never {
	error(503, { message: SESSION_CHECK_FAILED_MESSAGE });
}
