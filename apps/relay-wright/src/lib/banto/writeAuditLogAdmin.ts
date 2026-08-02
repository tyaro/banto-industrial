/**
 * Client for the W4 write-audit-log viewer (plan
 * `luminous-discovering-goblet.md`). Read-only: the engine is the ONLY writer
 * of the `write_audit_log` table; this surface just displays the trail.
 *
 * Same three-environment split as `auditLogAdmin.ts`:
 * - Tauri webview -> `invoke('write_audit_log_list', { params })`
 *   (server-side filter/sort/paginate via `ListParams`).
 * - LAN browser served by the embedded server -> `GET /api/write-audit-log`.
 *   That route returns the whole `ListResult` newest-first (it ignores request
 *   params by design - see its Rust doc comment), and the grid does its own
 *   client-side filter/sort/paginate over the rows, exactly as the W2 registry
 *   grids do over their GET-all lists.
 * - Plain `vite dev`/`vite preview` demo -> no audit DB, so every call rejects
 *   with `DEMO_MODE_MESSAGE`; `isWriteAuditLogAvailable()` lets the page show
 *   the note up front.
 *
 * Viewer+ on both paths (a read is never itself audited). Deliberately NOT
 * built on `@banto/admin-core`'s generic `DataProvider` - same reasoning as
 * `auditLogAdmin.ts` (a dedicated wire shape + its own Tauri command name).
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

/** Mirrors `relay_wright_core::write_audit_query::WriteAuditLogRow` (camelCase). */
export interface WriteAuditLogRow {
	id: number;
	ts: string;
	writeRuleId: number | null;
	ruleNameSnapshot: string;
	sourceTagId: number | null;
	sourceValueSnapshot: number | null;
	writeTargetId: number | null;
	targetValueWritten: number | null;
	actorUsername: string | null;
	/**
	 * One of `rule_fire`/`arm`/`disarm`/`dry_run_toggle`/`rate_limit_tripped`/
	 * `manual_write`（タグモニタのワンショット手動書き込み, feature/tag-monitor）.
	 */
	action: string;
	/**
	 * One of `ok`/`failed`/`suppressed_disarmed`/`suppressed_rate_limited`/
	 * `suppressed_dry_run`.
	 */
	result: string;
	detail: string | null;
}

export const DEMO_MODE_MESSAGE = 'デモモードでは利用できません';
const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

function demoModeError(): ProviderError {
	return new ProviderError({ kind: 'other', message: DEMO_MODE_MESSAGE });
}

/** Backed by a real audit DB (Tauri or the embedded server)? False in demo mode. */
export function isWriteAuditLogAvailable(): boolean {
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

/** Same token lookup as auditLogAdmin.ts - see that file's doc comment. */
function currentToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
}

async function httpGet<T>(path: string): Promise<T> {
	const headers: Record<string, string> = { ...CSRF_HEADER };
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;

	let response: Response;
	try {
		response = await fetch(path, { method: 'GET', headers });
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

/**
 * Filtered/sorted/paginated write-audit-log read (viewer+). On Tauri the
 * `params` reach SQL (true server-side paging); over REST the GET route returns
 * every row newest-first and the browser grid pages over them (see this
 * module's doc comment), so `params` is accepted uniformly but only load-bearing
 * on the Tauri path.
 */
export async function list(params: ListParams): Promise<ListResult<WriteAuditLogRow>> {
	if (!isWriteAuditLogAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri')
		return invokeCommand<ListResult<WriteAuditLogRow>>('write_audit_log_list', { params });
	return httpGet<ListResult<WriteAuditLogRow>>('/api/write-audit-log');
}
