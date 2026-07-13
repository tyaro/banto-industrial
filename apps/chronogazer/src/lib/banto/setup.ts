/**
 * Wires @banto/admin-core for the admin-template app (spec §3, §8, §11).
 * Imported once (side-effect) from the root layout, before any route guard
 * runs.
 *
 * M6 Phase B (spec §11.1): THREE environments are now distinguished, not
 * two:
 * 1. **Tauri webview** (`isTauri()`) — `TauriDataProvider`/
 *    `TauriAuthProvider` over `invoke()`, `TauriEventProvider` over the
 *    `banto://event` Tauri event (no network either way).
 * 2. **LAN browser served by the embedded server** (`isEmbeddedServer()`,
 *    async probe) — `HttpDataProvider`/`HttpAuthProvider` over `fetch()`
 *    against the same REST API `admin-template-core::rest` exposes, and
 *    `SseEventProvider` over `GET /api/events`. This is what a second
 *    machine on the LAN gets, and it's also what `banto-serve` (this repo's
 *    Tauri-free dev vehicle) serves.
 * 3. **Plain `vite dev`/`vite preview`** (neither of the above) — the M2
 *    Phase A `InMemoryDataProvider` + demo sessionStorage auth, so the app
 *    still runs with no Rust backend at all (tests, quick UI iteration).
 *
 * Detecting (2) requires an async network probe, so provider selection as a
 * whole is now async: `bantoReady` is the promise every entry point
 * (`routes/+layout.svelte`, the `(app)` route guard, the login page) awaits
 * before touching `getDataProvider()`/`getAuthProvider()`. The
 * resource/schema definitions and AuthProvider/DataProvider/EventProvider
 * contracts stay identical across all three - UI code never branches on
 * environment (spec §11.1).
 */
import {
	connectEvents,
	createHttpAuthProvider,
	createHttpDataProvider,
	createHttpUiSettings,
	createInMemoryDataProvider,
	createLocalUiSettings,
	createSseEventProvider,
	createTauriAuthProvider,
	createTauriDataProvider,
	createTauriEventProvider,
	createTauriUiSettings,
	initBanto
} from '@banto/admin-core';
import type {
	AuthProvider,
	Notifier,
	ResourceDefinition,
	UiSettingsProvider
} from '@banto/admin-core';
import type { FormSchema } from '@banto/forms';
// Safe to import in a plain browser (no Tauri runtime): only ever *called*
// when isTauri() is true.
import { invoke } from '@tauri-apps/api/core';
import { toastStore } from '$lib/toast.svelte';
import { sampleItems } from './sampleData';

const AUTH_KEY = 'banto.auth.demo';

/** True inside the Tauri webview, false in a plain browser tab (spec §11.1). */
export function isTauri(): boolean {
	return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

/** Shared with usersAdmin.ts (spec M10's `/api/users/*` calls need the same CSRF header every other fetch() here sends). */
export const CSRF_HEADER = { 'X-Banto-Client': 'banto' } as const;

/**
 * Which of the three spec §11.1 environments this tab ended up wired to -
 * `usersAdmin.ts` (spec M10) needs this to pick invoke() vs fetch() vs "not
 * available", the same three-way split `bantoReady` below already resolves,
 * just exposed as a plain synchronous read instead of re-running the async
 * probe. Set exactly once, at the end of whichever `bantoReady` branch runs;
 * defaults to 'demo' so a read before `bantoReady` resolves (should not
 * happen - see that promise's own doc comment) fails toward "unavailable"
 * rather than silently guessing 'tauri'/'server'.
 */
export type BantoMode = 'tauri' | 'server' | 'demo';
let bantoMode: BantoMode = 'demo';
export function getBantoMode(): BantoMode {
	return bantoMode;
}

/**
 * UI-settings persistence (spec §12.1, M12): mode-matched like the
 * data/auth providers above - Tauri -> `ui_settings_get/set` commands,
 * embedded server -> `/api/ui-settings/{key}` REST, plain-browser demo ->
 * localStorage. Defaults to the localStorage implementation so a read
 * before `bantoReady` resolves (e.g. `settings.svelte.ts`'s eager module
 * init) degrades to the local cache rather than crashing; the real
 * provider is swapped in by whichever `bantoReady` branch runs. Callers
 * treat writes as best-effort (unauthenticated writes fail server-side and
 * are swallowed - localStorage remains the always-written FOUC cache).
 */
let uiSettings: UiSettingsProvider = createLocalUiSettings();
export function getUiSettings(): UiSettingsProvider {
	return uiSettings;
}

/**
 * Is this plain-browser tab being served by the embedded Banto server
 * (`banto-server`/`admin-template-core::rest`, spec §11.1), as opposed to a
 * bare `vite dev`/`vite preview` tab with no Banto backend at all? Probed by
 * calling the one `/api` route that needs no auth token
 * (`GET /api/auth/check`): any HTTP response at all (`200` with a boolean
 * body when unauthenticated/authenticated, or an unexpected `401`/`403`)
 * means an `/api/*` route answered on the other end. A network error (no
 * server listening) or anything that isn't a plain HTTP response (e.g.
 * `vite dev`'s dev server 404ing with an HTML page for an unknown path)
 * means this is not our server. Never true inside Tauri - `isTauri()` is
 * checked first there and takes priority.
 */
async function isEmbeddedServer(): Promise<boolean> {
	if (isTauri()) return false;
	try {
		const response = await fetch(`${location.origin}/api/auth/check`, { headers: CSRF_HEADER });
		return response.status === 200 || response.status === 401;
	} catch {
		return false;
	}
}

// Rust's ItemInput.price/.stock (apps/admin-template/core/src/items.rs) are
// `i64`, so a fractional value must be rejected client-side too (not just
// bounds-checked) - otherwise it passes here and only fails after a round
// trip to the real backend. `validateField` (packages/forms/src/
// validate.ts) runs required, then min/max, then this `validate` in that
// order, so the built-in required/min/max checks still run first; this only
// adds an extra integer check on top.
const integerValidate = (value: unknown): string | null =>
	Number.isInteger(Number(value)) ? null : '整数で入力してください';

const itemsSchema: FormSchema = {
	fields: [
		{ name: 'name', label: '商品名', type: 'text', required: true, min: 1, max: 40 },
		{
			name: 'price',
			label: '価格',
			type: 'number',
			required: true,
			min: 0,
			max: 99999,
			validate: integerValidate
		},
		{
			name: 'stock',
			label: '在庫',
			type: 'number',
			required: true,
			min: 0,
			validate: integerValidate
		},
		{ name: 'updatedAt', label: '更新日', type: 'date', readonly: true }
	]
};

const itemsResource: ResourceDefinition = {
	name: 'items',
	label: '商品',
	icon: '📦',
	schema: itemsSchema,
	capabilities: { list: true, create: true, edit: true, delete: true }
};

const notifier: Notifier = { notify: (kind, message) => toastStore.push(kind, message) };

function isSessionAuthed(): boolean {
	return typeof sessionStorage !== 'undefined' && sessionStorage.getItem(AUTH_KEY) === '1';
}

/**
 * Demo AuthProvider (spec §3.3): fixed admin/admin credentials backed by
 * sessionStorage. Used in plain-browser dev only (mode 3 above); Tauri and
 * embedded-server modes use the real `auth_*` Rust commands/REST routes
 * instead (same admin/admin demo credentials, checked server-side).
 */
const demoAuthProvider: AuthProvider = {
	async login(params) {
		const { username, password } = params as { username?: string; password?: string };
		if (username === 'admin' && password === 'admin') {
			sessionStorage.setItem(AUTH_KEY, '1');
			return { success: true };
		}
		return { success: false, error: 'ユーザー名またはパスワードが違います' };
	},
	async logout() {
		sessionStorage.removeItem(AUTH_KEY);
	},
	async check() {
		return isSessionAuthed();
	},
	async getIdentity() {
		// Spec M10 RBAC: the demo provider's one fixed account is always
		// full 'admin' - this is the only environment where usersAdmin.ts is
		// unconditionally unavailable anyway (see isUsersAdminAvailable()),
		// so this only matters for permissions.ts-gated UI elsewhere (nav,
		// items page, settings page), which should behave exactly as if a
		// real admin were logged in.
		return isSessionAuthed() ? { id: 'admin', name: '管理者', role: 'admin' } : null;
	},
	// Always "initialized": the demo provider's admin/admin account always
	// exists, so the login page never shows the first-run setup form here
	// (spec §8.2's setup flow only applies to Tauri/embedded-server modes).
	async status() {
		return { initialized: true };
	},
	// No account store to change a password on in pure-browser demo mode;
	// the settings page hides the password-change section when this
	// resolves to `success: false` (see its "note" fallback).
	async changePassword() {
		return { success: false, error: 'デモモードでは変更できません' };
	}
};

/**
 * Resolves once `initBanto()` has run AND the matching `EventProvider` (if
 * any) is connected. Every place that calls `getDataProvider()`/
 * `getAuthProvider()` before the root layout has definitely mounted (the
 * `(app)` route guard's `load()`, the login page's submit handler) must
 * `await` this first; `routes/+layout.svelte` awaits it with `{#await}`
 * before rendering `children()` at all, so everything downstream of that is
 * already safe.
 */
export const bantoReady: Promise<void> = (async () => {
	if (isTauri()) {
		bantoMode = 'tauri';
		const dataProvider = createTauriDataProvider({ invoke });
		const authProvider = createTauriAuthProvider({ invoke });
		uiSettings = createTauriUiSettings({ invoke });
		initBanto({ dataProvider, authProvider, notifier, resources: [itemsResource] });

		// Dynamic import: @tauri-apps/api/event's `listen` talks to a real
		// Tauri IPC channel that does not exist outside the webview, so it must
		// not be evaluated at module load time in the other two environments.
		const { listen } = await import('@tauri-apps/api/event');
		connectEvents(createTauriEventProvider({ listen }));
		return;
	}

	if (await isEmbeddedServer()) {
		bantoMode = 'server';
		const authProvider = createHttpAuthProvider();
		const dataProvider = createHttpDataProvider({ getToken: authProvider.getToken });
		uiSettings = createHttpUiSettings({ getToken: authProvider.getToken });
		initBanto({ dataProvider, authProvider, notifier, resources: [itemsResource] });
		connectEvents(createSseEventProvider({ getToken: authProvider.getToken }));
		return;
	}

	// Plain `vite dev`/`vite preview`: no Banto backend at all, no EventProvider.
	bantoMode = 'demo';
	initBanto({
		dataProvider: createInMemoryDataProvider({ items: { rows: sampleItems } }),
		authProvider: demoAuthProvider,
		notifier,
		resources: [itemsResource]
	});
})();
