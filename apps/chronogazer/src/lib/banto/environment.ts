// banto v3.0.0（タグ v3.0.0 = f0dcece）の admin-template
// `apps/admin-template/src/lib/banto/environment.ts` からコピー（I2c、banto #286）。
// chronogazer 固有の差: なし（本文は無改変）。
/**
 * Environment detection for the three spec §11.1 runtime environments
 * (Tauri webview / embedded-server LAN browser / plain vite dev-preview).
 * Pure detection only - which providers get wired as a RESULT of these
 * checks lives in setup.ts (the composition root). Split out of setup.ts
 * (improvement-plan-2026-07.md P3-4) so app authors editing resources never
 * scroll through probe internals. Import these via `./setup` (which
 * re-exports them) unless you are setup.ts itself - keeping one public
 * entry point for the app.
 */

/** True inside the Tauri webview, false in a plain browser tab (spec §11.1). */
export function isTauri(): boolean {
	return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

/** Shared with the `*Admin.ts` modules (spec M10's `/api/users/*` calls need the same CSRF header every other fetch() here sends). */
export const CSRF_HEADER = { 'X-Banto-Client': 'banto' } as const;

/**
 * What the same-origin `GET /api/auth/check` probe found out (Issue #286).
 * Deployment kind ("which environment is this build served from") and
 * communication state ("did the probe get through right now") are different
 * questions; this three-way result keeps them apart:
 *
 * - `server`      - Banto's embedded server answered (LAN browser, `banto-serve`).
 * - `none`        - the origin answered, but it is definitively not Banto: a
 *                   static host / `vite preview` returning its own 404 for an
 *                   unknown `/api` path. That is the intended demo hosting.
 * - `unreachable` - no usable answer: network error, timeout, or a response
 *                   that is neither Banto's nor a plain "no such route" (a
 *                   reverse proxy's HTML 502/503 while the backend is down,
 *                   a bare 5xx...). Transient by nature - it MUST NOT be read
 *                   as "this is the demo" (that silently swaps a real backend
 *                   for in-memory sample data and never switches back).
 */
export type BackendProbe = 'server' | 'none' | 'unreachable';

/** Upper bound for one probe request; a hung connection must not pin the splash screen forever. */
export const PROBE_TIMEOUT_MS = 5000;

/**
 * Probes `GET /api/auth/check` (the one `/api` route that needs no token).
 *
 * - `200`/`401`, or any response carrying Banto's JSON error body
 *   (`{ "kind": ... }`, Issue #204: e.g. a `500` when the server could not
 *   check an account) -> `server`.
 * - `404`/`405`/`410` without that body -> `none` (a static host answering
 *   for itself).
 * - Everything else, including fetch exceptions and the timeout -> `unreachable`.
 *
 * Callers check `isTauri()` first (Tauri never probes). `fetchImpl` is
 * injectable for tests.
 */
export async function probeBackend(
	fetchImpl: typeof fetch = fetch,
	timeoutMs: number = PROBE_TIMEOUT_MS
): Promise<BackendProbe> {
	try {
		const response = await fetchImpl(`${location.origin}/api/auth/check`, {
			headers: CSRF_HEADER,
			signal: AbortSignal.timeout(timeoutMs)
		});
		if (response.status === 200 || response.status === 401) return 'server';
		const body: unknown = await response.json().catch(() => null);
		if (
			typeof body === 'object' &&
			body !== null &&
			typeof (body as { kind?: unknown }).kind === 'string'
		) {
			return 'server';
		}
		return response.status === 404 || response.status === 405 || response.status === 410
			? 'none'
			: 'unreachable';
	} catch {
		return 'unreachable';
	}
}

/**
 * Explicit "this build is the static demo" marker (Issue #286): set
 * `VITE_BANTO_DEMO=1` at build time (the GitHub Pages workflow does) and the
 * app goes straight to the in-memory demo providers without probing at all.
 * Builds without it (Tauri, the LAN-served `build/` embedded by banto-server)
 * never fall back to demo because of a failed request.
 */
export function isDemoBuild(): boolean {
	return import.meta.env.VITE_BANTO_DEMO === '1';
}

/**
 * Convenience wrapper: is this tab being served by Banto's embedded server?
 * `unreachable` counts as "no" here - startup (startup.ts) uses
 * {@link probeBackend} directly so it can tell "no server" from "not
 * reachable right now". Never true inside Tauri.
 */
export async function isEmbeddedServer(): Promise<boolean> {
	if (isTauri()) return false;
	return (await probeBackend()) === 'server';
}
