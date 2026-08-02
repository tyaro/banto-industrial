/**
 * Client for the project-file export/import API (feature `feature/project-file`):
 * save the whole configuration registry (PLC接続 / 収集グループ / タグ /
 * 書き込み先 / 書き込みルール / QR文字列) to a versioned JSON project file and
 * load it back. Same three-environment split as `writeRegistryAdmin.ts`:
 *
 * - Tauri webview -> `invoke()` the `project_export` / `project_import`
 *   commands (`apps/relay-wright/src-tauri/src/lib.rs`).
 * - LAN browser served by the embedded server -> `fetch()` the
 *   `/api/project/export` / `/api/project/import` REST routes
 *   (`apps/relay-wright/core/src/rest.rs`).
 * - Plain `vite dev`/`vite preview` demo -> no backend, so every call rejects
 *   with `DEMO_MODE_MESSAGE`; `isProjectAvailable()` lets the page note it.
 *
 * Export is editor+ (a read of non-secret config); import is admin-only,
 * refused while the engine is armed, and REPLACES the entire configuration -
 * the page role-gates the controls (`canWriteResources` / `isAdmin`) and the
 * backend enforces the same (invariant §1 両経路対称).
 */
import { invoke } from '@tauri-apps/api/core';
import { getAuthProvider, isProviderError, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER, getBantoMode } from './setup';
import type { PlcConnection, CollectionGroup, Tag } from './tagRegistryAdmin';
import type { WriteTarget, WriteRuleDetail } from './writeRegistryAdmin';
import type { QrString } from './qrAdmin';

// --- wire types (camelCase, matching the Rust serde shapes) -----------------

/** Mirrors `relay_wright_core::project::FORMAT`. */
export const PROJECT_FORMAT = 'relay-wright-project';
/** Mirrors `relay_wright_core::project::VERSION`. */
export const PROJECT_VERSION = 1;

/** Mirrors `relay_wright_core::project::ProjectFile`. */
export interface ProjectFile {
	format: string;
	version: number;
	exportedAt?: string | null;
	appVersion?: string | null;
	plcConnections: PlcConnection[];
	collectionGroups: CollectionGroup[];
	tags: Tag[];
	writeTargets: WriteTarget[];
	writeRules: WriteRuleDetail[];
	qrStrings: QrString[];
}

/** Mirrors `relay_wright_core::project::ImportSummary`. */
export interface ImportSummary {
	plcConnections: number;
	collectionGroups: number;
	tags: number;
	writeTargets: number;
	writeRules: number;
	writeRuleConditions: number;
	qrStrings: number;
}

// --- environment/error plumbing (mirrors writeRegistryAdmin.ts) -------------

export const DEMO_MODE_MESSAGE = 'デモモードでは利用できません';
const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

function demoModeError(): ProviderError {
	return new ProviderError({ kind: 'other', message: DEMO_MODE_MESSAGE });
}

/** Backed by a real registry DB (Tauri or embedded server)? False in demo mode. */
export function isProjectAvailable(): boolean {
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

function currentToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
}

interface HttpInit {
	method: string;
	body?: unknown;
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

// --- export / import --------------------------------------------------------

/** Export the whole configuration as a project file (editor+). */
export async function exportProject(): Promise<ProjectFile> {
	if (!isProjectAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<ProjectFile>('project_export');
	return httpRequest<ProjectFile>('/api/project/export', { method: 'GET' });
}

/**
 * REPLACE the entire configuration with `project` (admin-only). Rejected while
 * the engine is armed. Returns the per-table counts applied.
 */
export async function importProject(project: ProjectFile): Promise<ImportSummary> {
	if (!isProjectAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<ImportSummary>('project_import', { project });
	}
	return httpRequest<ImportSummary>('/api/project/import', { method: 'POST', body: project });
}
