/**
 * Client for the W3-B2 auto-write engine control surface (plan
 * `luminous-discovering-goblet.md`, W4 monitoring/operation UI). Same
 * three-environment split as `writeRegistryAdmin.ts`/`auditLogAdmin.ts`:
 *
 * - Tauri webview -> `invoke()` the `engine_arm`/`engine_disarm`/
 *   `engine_set_dry_run`/`engine_status`/`engine_reload` commands
 *   (`apps/relay-wright/src-tauri/src/lib.rs`).
 * - LAN browser served by the embedded server -> `fetch()` the
 *   `/api/engine/arm|disarm|dry-run|status` REST routes
 *   (`apps/relay-wright/core/src/rest.rs`).
 * - Plain `vite dev`/`vite preview` demo -> no engine at all, so every call
 *   rejects with `DEMO_MODE_MESSAGE`; `isEngineAvailable()` lets the page show
 *   the note up front.
 *
 * `reload` is Tauri-ONLY: it tears down and rebuilds the `Engine` object owned
 * by the desktop app's `AppState`, which the REST state cannot reach (see the
 * `engine_router` doc comment - there is deliberately NO `POST /api/engine/
 * reload`). `isEngineReloadAvailable()` is false outside Tauri so the page can
 * hide/disable the reload control and surface 「デスクトップ版のみ」.
 *
 * RBAC (invariant §1 両経路対称, enforced on the backend too): arm/disarm/
 * reload = admin, dry-run = editor, status = viewer+. The page role-gates the
 * controls via `permissions.ts`; the backend is the authority.
 */
import { invoke } from '@tauri-apps/api/core';
import { getAuthProvider, isProviderError, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER, getBantoMode } from './setup';

/** Mirrors `relay_wright_core::engine::EngineStatus` (camelCase on the wire). */
export interface EngineStatus {
	armed: boolean;
	dryRun: boolean;
	/**
	 * The persisted armed state observed at startup - informational only. The
	 * engine NEVER auto-resumes live writing (invariant §1 / safety design), so
	 * this drives only the "前回はアーム状態でした" banner, never behavior.
	 */
	wasArmedBeforeRestart: boolean;
}

export const DEMO_MODE_MESSAGE = 'デモモードでは利用できません';
export const RELOAD_DESKTOP_ONLY_MESSAGE = 'この操作はデスクトップ版のみで利用できます';
const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

function demoModeError(): ProviderError {
	return new ProviderError({ kind: 'other', message: DEMO_MODE_MESSAGE });
}

/** Backed by a real engine (Tauri or the embedded server)? False in demo mode. */
export function isEngineAvailable(): boolean {
	return getBantoMode() !== 'demo';
}

/**
 * Is engine reload usable here? Only in the Tauri desktop app - reload rebuilds
 * the in-process `Engine`, which has no REST route (see this module's doc
 * comment). The page uses this to hide/disable the reload control elsewhere.
 */
export function isEngineReloadAvailable(): boolean {
	return getBantoMode() === 'tauri';
}

const ERROR_KINDS = new Set([
	'not_found',
	'validation',
	'unauthorized',
	'forbidden',
	'storage',
	'other'
]);

function isErrorBody(value: unknown): value is ErrorBody {
	if (typeof value !== 'object' || value === null) return false;
	const kind = (value as { kind?: unknown }).kind;
	return typeof kind === 'string' && ERROR_KINDS.has(kind);
}

function toProviderError(err: unknown): ProviderError {
	if (isProviderError(err)) return err;
	if (isErrorBody(err)) return new ProviderError(err);
	const message = err instanceof Error ? err.message : String(err);
	return new ProviderError({ kind: 'other', message });
}

async function invokeCommand<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	try {
		return (await invoke(cmd, args)) as T;
	} catch (err) {
		throw toProviderError(err);
	}
}

/** Same token lookup as writeRegistryAdmin.ts - see that file's doc comment. */
function currentToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
}

interface HttpInit {
	method: string;
	body?: unknown;
	expectNoContent?: boolean;
}

async function httpRequest<T>(path: string, init: HttpInit): Promise<T> {
	const hasBody = init.body !== undefined;
	const headers: Record<string, string> = { ...CSRF_HEADER };
	if (hasBody) headers['Content-Type'] = 'application/json';
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;

	let response: Response;
	try {
		response = await fetch(path, {
			method: init.method,
			headers,
			body: hasBody ? JSON.stringify(init.body) : undefined
		});
	} catch {
		throw new ProviderError({ kind: 'other', message: NETWORK_ERROR_MESSAGE });
	}

	if (!response.ok) {
		let body: unknown;
		try {
			body = await response.json();
		} catch {
			throw new ProviderError({
				kind: 'other',
				message: `${response.status} ${response.statusText}`
			});
		}
		if (isErrorBody(body)) throw new ProviderError(body);
		throw new ProviderError({
			kind: 'other',
			message: `${response.status} ${response.statusText}`
		});
	}

	if (init.expectNoContent) return undefined as T;
	return (await response.json()) as T;
}

/** The engine's arm/dry-run snapshot (viewer+). */
export async function getStatus(): Promise<EngineStatus> {
	if (!isEngineAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<EngineStatus>('engine_status');
	return httpRequest<EngineStatus>('/api/engine/status', { method: 'GET' });
}

/**
 * Arm the engine: enable live physical writes (admin). The page MUST gate this
 * behind an explicit confirmation dialog - once armed, a satisfied rule writes
 * to the real PLC automatically.
 */
export async function arm(): Promise<void> {
	if (!isEngineAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('engine_arm');
		return;
	}
	await httpRequest<void>('/api/engine/arm', { method: 'POST', expectNoContent: true });
}

/** Disarm the engine: suppress all physical writes (admin). */
export async function disarm(): Promise<void> {
	if (!isEngineAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('engine_disarm');
		return;
	}
	await httpRequest<void>('/api/engine/disarm', { method: 'POST', expectNoContent: true });
}

/** Turn dry-run on/off (editor). Dry-run can only make the engine safer. */
export async function setDryRun(on: boolean): Promise<void> {
	if (!isEngineAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('engine_set_dry_run', { on });
		return;
	}
	await httpRequest<void>('/api/engine/dry-run', {
		method: 'POST',
		body: { on },
		expectNoContent: true
	});
}

/**
 * Rebuild the engine from the current DB (enabled connections + rules), admin,
 * Tauri-ONLY. Returns the post-reload status (always disarmed - a reload never
 * auto-arms). Rejects with `RELOAD_DESKTOP_ONLY_MESSAGE` outside Tauri; callers
 * should also check `isEngineReloadAvailable()` and hide/disable the control.
 */
export async function reload(): Promise<EngineStatus> {
	if (!isEngineAvailable()) throw demoModeError();
	if (getBantoMode() !== 'tauri') {
		throw new ProviderError({ kind: 'other', message: RELOAD_DESKTOP_ONLY_MESSAGE });
	}
	return invokeCommand<EngineStatus>('engine_reload');
}
