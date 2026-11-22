/**
 * Wires @banto/admin-core for the admin-template app (spec §3, §8, §11).
 * Imported once (side-effect) from the root layout, before any route guard
 * runs.
 *
 * M6 Phase B (spec §11.1): the environments distinguished are:
 * 1. **Tauri webview** (`isTauri()`) — `TauriDataProvider`/
 *    `TauriAuthProvider` over `invoke()`, `TauriEventProvider` over the
 *    `banto://event` Tauri event (no network either way).
 * 2. **LAN browser served by the embedded server** (`probeBackend()`,
 *    async probe; a failed probe is retried/surfaced, never read as an
 *    offline mode — Issue #286, `startup.ts`) — `HttpDataProvider`/`HttpAuthProvider` over
 *    `fetch()` against the same REST API `admin-template-core::rest` exposes, and
 *    `SseEventProvider` over `GET /api/events`. This is what a second
 *    machine on the LAN gets, and it's also what `banto-serve` (this repo's
 *    Tauri-free dev vehicle) serves.
 * 3. **The GitHub Pages demo** (`VITE_BANTO_DEMO=1` build) — no probe; `#lib/demo/`
 *    swaps fetch/WebSocket for an in-tab mock first, then this is wired as 2
 *    (docs/demo-pages-design.md). The old in-memory demo mode (`providers/demo.ts`,
 *    admin/admin login, everything refused) is gone (§3.2, §6 の 4): a plain
 *    `vite dev`/static host with no backend is "cannot connect", like Hub.
 *
 * Detecting (2) requires an async network probe, so provider selection as a
 * whole is async: `bantoReady` is the promise every entry point
 * (`routes/+layout.svelte`, the login page) awaits
 * before touching `getDataProvider()`/`getAuthProvider()`. The
 * resource/schema definitions and AuthProvider/DataProvider/EventProvider
 * contracts stay identical across them - UI code never branches on
 * environment (spec §11.1).
 *
 * banto v3.0.0（#286、I2c）: 環境の判定は admin-template と同じ形にした。
 * 判定そのもの（`isTauri` / `probeBackend` / `isDemoBuild` / `CSRF_HEADER`）
 * は `environment.ts`、判定の流れ（`resolveStartupTarget`）は `startup.ts`、
 * 起動待ちの画面の状態は `startupState.svelte.ts`（いずれも banto v3.0.0 の
 * 写し）。以前の `isEmbeddedServer()` は fetch の失敗・例外を「サーバー無し」
 * と読み、LAN ビルドで一時的に届かないだけでも demo（メモリ上の空データ）に
 * 落ちて二度と戻らなかった。今は旧 demo 自体を無くした（docs/demo-pages-design.md
 * §3.2）。`VITE_BANTO_DEMO=1` のビルドは模擬のサーバーを入れて `server`、それ以外で
 * 届かない（404 の静的ホスト・`vite dev` も含む）ときは起動待ちで再試行し、
 * 届かなければ「サーバーに接続できません」と再接続ボタンを出す。
 * 外へ出す名前（`isTauri` / `CSRF_HEADER` / `getBantoMode` など）はこのファイル
 * から再エクスポートし、`*Admin.ts` の import は変えない。
 *
 * banto v3.0.1（#321）: `(app)` のガードは `bantoReady` を待たず、`isBantoReady()`
 * で終わっていなければ起動待ちの印付きの 503 を投げて延期する
 * （`startupGate.ts`）。保護画面を直接開いたとき、届かない間も真っ白にならず
 * スプラッシュと再接続が出る。
 */
import {
	connectEvents,
	createHttpAuthProvider,
	createHttpDataProvider,
	createHttpUiSettings,
	createLocalUiSettings,
	createSseEventProvider,
	createTauriAuthProvider,
	createTauriDataProvider,
	createTauriEventProvider,
	createTauriUiSettings,
	initBanto
} from '@banto/admin-core';
import type { Notifier, UiSettingsProvider } from '@banto/admin-core';
// Safe to import in a plain browser (no Tauri runtime): only ever *called*
// when isTauri() is true.
import { invoke } from '@tauri-apps/api/core';
import { toastStore } from '#lib/toast.svelte.js';
import { CSRF_HEADER, isDemoBuild, isTauri, probeBackend } from './environment';
import { resolveStartupTarget } from './startup';
import { setStartupStatus, waitForStartupRetry } from './startupState.svelte';

// Re-exported so the rest of the app keeps importing from './setup' (one
// public entry point; the split into environment.ts is an internal detail).
export { CSRF_HEADER, isTauri };

/**
 * Which environment this tab ended up wired to - `usersAdmin.ts` (spec M10)
 * etc. need this to pick invoke() vs fetch(), the same two-way split
 * `bantoReady` below resolves, exposed as a plain synchronous read instead of
 * re-running the async probe. Set exactly once, at the end of whichever
 * `bantoReady` branch runs (the GitHub Pages demo is `server` too).
 *
 * Default before `bantoReady` resolves: `'server'`. A read that early should
 * not happen (see that promise's doc comment; the `(app)` guard defers until
 * ready). The old default `'demo'` was the "unavailable" value, but there is no
 * such mode any more, and `'tauri'` would guess an `invoke()` that does not
 * exist in a browser tab. `'server'` is the fail-safe side: the `*Admin.ts`
 * modules then use `fetch()` with no token wired yet, so the call is refused
 * (401 / cannot connect) instead of silently succeeding with empty sample data.
 */
export type BantoMode = 'tauri' | 'server';
let bantoMode: BantoMode = 'server';
export function getBantoMode(): BantoMode {
	return bantoMode;
}

/**
 * UI-settings persistence (spec §12.1, M12): mode-matched like the
 * data/auth providers above - Tauri -> `ui_settings_get/set` commands,
 * embedded server (and the Pages demo's mock) -> `/api/ui-settings/{key}` REST. Defaults to the localStorage implementation (before the real provider is wired) so a read
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

const notifier: Notifier = { notify: (kind, message) => toastStore.push(kind, message) };

/** Set once {@link bantoReady} resolves - see {@link isBantoReady}. */
let ready = false;

/**
 * Resolves once `initBanto()` has run AND the matching `EventProvider` (if
 * any) is connected. Every place that calls `getDataProvider()`/
 * `getAuthProvider()` before the root layout has definitely mounted (the
 * login page's submit handler) must `await` this first;
 * `routes/+layout.svelte` awaits it with `{#await}` before rendering
 * `children()` at all, so everything downstream of that is already safe.
 *
 * banto v3.0.1（#321）: a route guard must NOT simply `await` this while it is
 * pending - on a first load SvelteKit renders nothing (not even the root
 * layout's splash) until every load has finished, and while the server is
 * unreachable this waits for the user's "reconnect" on that very splash. The
 * `(app)` guard checks {@link isBantoReady} instead and defers
 * (`startupGate.ts`).
 */
export const bantoReady: Promise<void> = (async () => {
	// デモ（GitHub Pages、docs/demo-pages-design.md §3.1・§3.2）: 模擬を入れ終えてから、
	// `server` モードの通常の provider（HTTP）を作る。下の `resolveStartupTarget` はデモの
	// ビルドでは probe せずに `server` を返す（本物のサーバーへ問い合わせない）。
	// `import.meta.env.VITE_BANTO_DEMO` はビルドの時点で定数になるので、製品のビルドでは
	// この分岐と `#lib/demo/` の chunk ごと消える。模擬の import か初期化が失敗したら、
	// ここで reject して通常の provider を作らない（root layout が止まった画面を出す）。
	let demoTokenStorageKey: string | undefined;
	if (import.meta.env.VITE_BANTO_DEMO === '1') {
		const demo = await import('#lib/demo/index.js');
		demoTokenStorageKey = (await demo.startDemo()).tokenStorageKey;
	}

	// Deployment kind is decided explicitly (Tauri / `VITE_BANTO_DEMO` build /
	// a probe answer from Banto's server); a failed probe (incl. a static host's
	// 404) stays on the splash screen with a retry button until the server
	// answers (Issue #286, startup.ts).
	const target = await resolveStartupTarget({
		isTauri,
		isDemoBuild,
		probe: () => probeBackend(),
		sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
		setStatus: setStartupStatus,
		waitForRetry: waitForStartupRetry
	});

	if (target === 'tauri') {
		bantoMode = 'tauri';
		const dataProvider = createTauriDataProvider({ invoke });
		const authProvider = createTauriAuthProvider({ invoke });
		uiSettings = createTauriUiSettings({
			invoke,
			getSessionRevision: () => authProvider.credentialRevision()
		});
		initBanto({ dataProvider, authProvider, notifier, resources: [] });

		// Dynamic import: @tauri-apps/api/event's `listen` talks to a real
		// Tauri IPC channel that does not exist outside the webview, so it must
		// not be evaluated at module load time in the other two environments.
		const { listen } = await import('@tauri-apps/api/event');
		connectEvents(createTauriEventProvider({ listen }));
		return;
	}

	// target === 'server'（LAN ブラウザ、または模擬を入れた Pages のデモ）。
	bantoMode = 'server';
	const authProvider = createHttpAuthProvider(
		demoTokenStorageKey === undefined ? {} : { storageKey: demoTokenStorageKey }
	);
	const dataProvider = createHttpDataProvider({ getToken: authProvider.getToken });
	uiSettings = createHttpUiSettings({ getToken: authProvider.getToken });
	initBanto({ dataProvider, authProvider, notifier, resources: [] });
	connectEvents(createSseEventProvider({ getToken: authProvider.getToken }));
})().then(() => {
	ready = true;
});

/**
 * Whether {@link bantoReady} has resolved (banto v3.0.1、#321): a synchronous
 * read for the `(app)` route guard, which must decide WITHOUT waiting whether
 * it can run now or has to defer to the startup splash (`startupGate.ts`). Set
 * in the promise's own continuation, so it is already true for anything
 * chained on `bantoReady` (the root layout's re-run).
 */
export function isBantoReady(): boolean {
	return ready;
}
