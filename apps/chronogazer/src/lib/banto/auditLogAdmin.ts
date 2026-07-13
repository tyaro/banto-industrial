/**
 * Client for the `admin`-only audit-log API (spec M14, `docs/roadmap.md`).
 * Same Tauri/REST split as `usersAdmin.ts`: the Tauri webview calls
 * `invoke()` directly (the `audit_log_list`/`audit_config_get`/
 * `audit_config_apply` commands, `apps/admin-template/src-tauri/src/lib.rs`),
 * a LAN browser client served by the embedded server calls `fetch()` against
 * `/api/audit-log/*` (`apps/admin-template/core/src/rest.rs`), reusing the
 * same bearer-token/CSRF-header mechanism `@banto/admin-core`'s
 * `createHttpDataProvider`/`createHttpAuthProvider` use.
 *
 * Deliberately NOT built on `@banto/admin-core`'s generic
 * `DataProvider`/`getDataProvider()` - same reasoning as `usersAdmin.ts`:
 * this is a small, dedicated surface (one list read + one settings read/
 * write) with its own Tauri command names, not a `{resource}_list`-shaped
 * CRUD resource.
 *
 * Plain `vite dev`/`vite preview` (spec §11.1's third environment, no Rust
 * backend at all): there is no audit-log database to read, so every export
 * here throws/rejects with a `ProviderError` carrying `DEMO_MODE_MESSAGE`,
 * mirroring `usersAdmin.ts`'s `isUsersAdminAvailable()`/`demoModeError()`.
 */
import { invoke } from '@tauri-apps/api/core';
import {
	getAuthProvider,
	isProviderError,
	ProviderError,
	type ErrorBody,
	type ListParams,
	type ListResult
} from '@banto/admin-core';
import { CSRF_HEADER, getBantoMode } from './setup';

/** Mirrors `admin_template_core::audit::AuditLogEntry` (camelCase on the wire). */
export interface AuditLogEntry {
	id: number;
	ts: string;
	actorUsername: string | null;
	actorRole: string | null;
	action: string;
	resource: string;
	entityId: string | null;
	/** Raw JSON-encoded summary string, as stored - `JSON.parse` on demand for display. */
	detail: string | null;
	origin: string;
	result: string;
}

/** Mirrors `admin_template_core::settings::AuditSettings` (camelCase on the wire). `null` on either field means unlimited on that dimension (spec M14: "0以下は無制限"). */
export interface AuditSettings {
	retentionDays: number | null;
	retentionRows: number | null;
}

export const DEMO_MODE_MESSAGE = 'デモモードでは利用できません';

function demoModeError(): ProviderError {
	return new ProviderError({ kind: 'other', message: DEMO_MODE_MESSAGE });
}

/** Is this environment backed by a real audit-log database (Tauri or the embedded server)? False in plain-browser demo mode. */
export function isAuditLogAvailable(): boolean {
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

/** Same type guard as providers/tauri.ts / providers/http.ts / usersAdmin.ts (spec §10/§11.1). */
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

const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

interface HttpInit {
	method: string;
	body?: unknown;
}

/** Same token lookup as usersAdmin.ts - see that file's doc comment. */
function currentToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
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

	return (await response.json()) as T;
}

/** Filtered/sorted/paginated audit-log read (spec M14's admin-only viewer). */
export async function listAuditLog(params: ListParams): Promise<ListResult<AuditLogEntry>> {
	if (!isAuditLogAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri')
		return invokeCommand<ListResult<AuditLogEntry>>('audit_log_list', { params });
	return httpRequest<ListResult<AuditLogEntry>>('/api/audit-log/list', {
		method: 'POST',
		body: params
	});
}

/** Current audit-log retention policy. Any authenticated role may call this (it only feeds a settings-screen display) - see `audit_config_get`'s Rust doc comment. */
export async function getAuditConfig(): Promise<AuditSettings> {
	if (!isAuditLogAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<AuditSettings>('audit_config_get');
	return httpRequest<AuditSettings>('/api/audit-log/config', { method: 'GET' });
}

/** Persist a new retention policy. `admin`-only (rejected with a `forbidden` `ProviderError` otherwise). */
export async function setAuditConfig(config: AuditSettings): Promise<AuditSettings> {
	if (!isAuditLogAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<AuditSettings>('audit_config_apply', {
			retentionDays: config.retentionDays,
			retentionRows: config.retentionRows
		});
	}
	return httpRequest<AuditSettings>('/api/audit-log/config', { method: 'PUT', body: config });
}
