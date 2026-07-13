/**
 * Client for the `items` bulk-CSV-import API (spec M15 Phase C, `docs/roadmap.md`).
 * Same Tauri/REST split as `usersAdmin.ts`/`auditLogAdmin.ts`: the Tauri
 * webview calls `invoke('items_import', { rows })` directly
 * (`apps/admin-template/src-tauri/src/lib.rs`'s `items_import` command, whose
 * single non-state argument is named `rows: Vec<ItemImportRow>`), a LAN
 * browser client served by the embedded server calls
 * `POST /api/items/import` (`apps/admin-template/core/src/rest.rs`) with the
 * row array as the request body directly - NOT wrapped in `{ rows: [...] }` -
 * matching that handler's `Json(rows): Json<Vec<ItemImportRow>>` extractor.
 *
 * Everyday items CRUD (list/create/update/delete) goes through
 * `@banto/admin-core`'s generic `DataProvider`/`getDataProvider()` instead
 * (see `+page.svelte`/`ItemsServerGrid.svelte`) since it fits that resource-
 * CRUD convention perfectly. Bulk import doesn't: it's a one-off endpoint
 * with its own row-shaped request/response, not a `{resource}_list`-style
 * call - a small dedicated client (this file) mirrors `usersAdmin.ts` rather
 * than bending the generic contract to fit.
 *
 * Plain `vite dev`/`vite preview` (spec §11.1's third environment, no Rust
 * backend at all): ordinary items CRUD still works there against an
 * `InMemoryDataProvider`, but there is no bulk-import endpoint equivalent -
 * `importItems()` throws/rejects with a `ProviderError` carrying
 * `DEMO_MODE_MESSAGE`; `isItemsImportAvailable()` lets the items page check
 * this up front, mirroring `isUsersAdminAvailable()`.
 */
import { invoke } from '@tauri-apps/api/core';
import { getAuthProvider, isProviderError, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER, getBantoMode } from './setup';

/**
 * Mirrors `admin_template_core::items::ItemImportRow` (camelCase on the
 * wire). `id` present selects UPDATE, absent selects INSERT (spec M15).
 */
export interface ItemImportRow {
	id?: number;
	name: string;
	price: number;
	stock: number;
}

/**
 * One row-level failure, mirrors `admin_template_core::items::ImportRowError`.
 * `row` is the 0-based index into the request's row array (NOT a database
 * id, NOT a 1-based CSV line number) - callers building a CSV-line-numbered
 * preview must add back the header line + 1-based offset themselves.
 */
export interface ImportRowError {
	row: number;
	message: string;
}

/** Mirrors `admin_template_core::items::ImportResult`. Non-empty `errors` means the whole batch was rolled back (`created`/`updated` are then always `0`). */
export interface ImportResult {
	created: number;
	updated: number;
	errors: ImportRowError[];
}

export const DEMO_MODE_MESSAGE = 'デモモードでは利用できません';

function demoModeError(): ProviderError {
	return new ProviderError({ kind: 'other', message: DEMO_MODE_MESSAGE });
}

/** Is bulk CSV import backed by a real server (Tauri or the embedded server)? False in plain-browser demo mode - see this module's doc comment. */
export function isItemsImportAvailable(): boolean {
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

/** Same token lookup as usersAdmin.ts/auditLogAdmin.ts - see those files' doc comments. */
function currentToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
}

async function httpPostJson<T>(path: string, body: unknown): Promise<T> {
	const headers: Record<string, string> = { ...CSRF_HEADER, 'Content-Type': 'application/json' };
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;

	let response: Response;
	try {
		response = await fetch(path, { method: 'POST', headers, body: JSON.stringify(body) });
	} catch {
		throw new ProviderError({ kind: 'other', message: NETWORK_ERROR_MESSAGE });
	}

	if (!response.ok) {
		let errBody: unknown;
		try {
			errBody = await response.json();
		} catch {
			throw new ProviderError({
				kind: 'other',
				message: `${response.status} ${response.statusText}`
			});
		}
		if (isErrorBody(errBody)) throw new ProviderError(errBody);
		throw new ProviderError({
			kind: 'other',
			message: `${response.status} ${response.statusText}`
		});
	}

	return (await response.json()) as T;
}

/**
 * Bulk create/update `items` from parsed CSV rows (spec M15). `editor`+ only
 * (enforced server-side - the items page's `canWrite` check should already
 * have hidden this UI for `viewer`). All-or-nothing: a non-empty
 * `result.errors` means the whole batch was rolled back server-side, so
 * `created`/`updated` are `0` in that case.
 */
export async function importItems(rows: ItemImportRow[]): Promise<ImportResult> {
	if (!isItemsImportAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<ImportResult>('items_import', { rows });
	return httpPostJson<ImportResult>('/api/items/import', rows);
}
