/**
 * Client for the タグモニタ screen (feature/tag-monitor). Same
 * three-environment split as `engineAdmin.ts`/`tagRegistryAdmin.ts`:
 *
 * - Tauri webview -> `invoke()` the `monitor_group_read`/`monitor_tag_write`
 *   commands (`apps/relay-wright/src-tauri/src/lib.rs`).
 * - LAN browser served by the embedded server -> `fetch()` the
 *   `POST /api/monitor/read|write` REST routes
 *   (`apps/relay-wright/core/src/rest.rs`).
 * - Plain `vite dev`/`vite preview` demo -> no engine/PLC at all, so every
 *   call rejects with `DEMO_MODE_MESSAGE`; `isMonitorAvailable()` lets the
 *   page show the note up front.
 *
 * Both paths ride the engine broker's one-session-per-connection SLMP tasks
 * (the real R08ENCPU accepts only ONE concurrent SLMP connection per
 * connected port - the monitor never opens a second one of its own), so
 * values here are read over the SAME socket the
 * auto-write engine polls through.
 *
 * RBAC (invariant §1 両経路対称, backend is the authority): read = viewer+,
 * manual write = editor+. Manual writes are the user's explicitly relaxed
 * DEBUG path - no arm gate, no confirm dialog - but every attempt is audited
 * server-side (`write_audit_log`, action `manual_write`).
 */
import { invoke } from '@tauri-apps/api/core';
import { getAuthProvider, isProviderError, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER, getBantoMode } from './setup';

/** Mirrors `relay_wright_core::engine::MonitorValue` (camelCase on the wire). */
export interface MonitorValue {
	tagId: number;
	tagName: string;
	address: string;
	dataType: string;
	unit: string | null;
	/** Number (numeric/bit tags; bit is 0/1) or string (string tags); null when bad. */
	value: number | string | null;
	quality: 'good' | 'bad';
	error: string | null;
}

export const DEMO_MODE_MESSAGE = 'デモモードでは利用できません';
const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

/**
 * The backend's "engine never started" message (`engine_control_now` /
 * `current_engine_control`). The page matches on this to show the dedicated
 * banner instead of a per-poll toast storm.
 */
export const ENGINE_NOT_RUNNING_MESSAGE = '自動書き込みエンジンが起動していません';

function demoModeError(): ProviderError {
	return new ProviderError({ kind: 'other', message: DEMO_MODE_MESSAGE });
}

/** Backed by a real engine (Tauri or the embedded server)? False in demo mode. */
export function isMonitorAvailable(): boolean {
	return getBantoMode() !== 'demo';
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

/** Same token lookup as engineAdmin.ts - see that file's doc comment. */
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

/**
 * The selected 収集グループ's tags as display-ready realtime values
 * (viewer+). Per-tag quality: a dead address/session shows as
 * `quality: 'bad'` entries, never an exception, so the poll loop keeps
 * rendering.
 */
export async function readGroup(collectionGroupId: number): Promise<MonitorValue[]> {
	if (!isMonitorAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<MonitorValue[]>('monitor_group_read', { collectionGroupId });
	}
	return httpRequest<MonitorValue[]>('/api/monitor/read', {
		method: 'POST',
		body: { collectionGroupId }
	});
}

/**
 * One-shot manual write to a tag's device (editor+). The value is the
 * ENGINEERING value as typed (the backend unscales it to raw); bit tags take
 * 0/1/true/false, string tags Shift-JIS text. No arm gate and no confirm -
 * the user's explicit relaxation for this debug screen - but every attempt is
 * audited server-side.
 */
export async function writeTag(tagId: number, value: string): Promise<void> {
	if (!isMonitorAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('monitor_tag_write', { tagId, value });
		return;
	}
	await httpRequest<void>('/api/monitor/write', {
		method: 'POST',
		body: { tagId, value },
		expectNoContent: true
	});
}
