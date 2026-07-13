/**
 * Client for the `admin`-only SQLite backup/restore API (spec M17,
 * `docs/roadmap.md`). Same Tauri/REST split as `usersAdmin.ts`/
 * `auditLogAdmin.ts`: the Tauri webview calls `invoke()` directly (the
 * `backups_*` commands, `apps/admin-template/src-tauri/src/lib.rs`), a LAN
 * browser client served by the embedded server calls `fetch()` against
 * `/api/backups/*` (`apps/admin-template/core/src/rest.rs`), reusing the
 * same bearer-token/CSRF-header mechanism `@banto/admin-core`'s
 * `createHttpDataProvider`/`createHttpAuthProvider` use.
 *
 * Deliberately NOT built on `@banto/admin-core`'s generic
 * `DataProvider`/`getDataProvider()` - same reasoning as `usersAdmin.ts`:
 * this is a small, dedicated surface with its own Tauri command names and
 * REST shapes (one of which is a raw-bytes file download/upload, not JSON),
 * not a `{resource}_list`-shaped CRUD resource.
 *
 * Two operations are mode-restricted, not just mode-branched (spec M17's UI
 * bullet "LAN時のみ「ファイルからリストア」" plus this app's own design
 * call for downloads - see each function's doc comment):
 * - `downloadBackup` only works in REST/LAN mode. In Tauri there is no
 *   browser download mechanism to speak of - the desktop equivalent is
 *   `openBackupsFolder` (Windows Explorer on the real `backups/` dir), so
 *   this rejects with a message pointing the caller at that button instead.
 * - `uploadAndStageRestore` only works in REST/LAN mode (spec: "アップロード
 *   or 一覧から選択" but only the LAN browser can arbitrarily pick a local
 *   file to upload over HTTP). Tauri restores are staged from the existing
 *   `backups/` list only, via `stageRestoreFromBackup`.
 *
 * Plain `vite dev`/`vite preview` (spec §11.1's third environment, no Rust
 * backend at all): there is no database file to back up, so every export
 * here throws/rejects with a `ProviderError` carrying `DEMO_MODE_MESSAGE`,
 * mirroring `usersAdmin.ts`'s `isUsersAdminAvailable()`/`demoModeError()`.
 */
import { invoke } from '@tauri-apps/api/core';
import { getAuthProvider, isProviderError, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER, getBantoMode } from './setup';

/** Mirrors `admin_template_core::backup::BackupInfo` (camelCase on the wire). */
export interface BackupInfo {
	fileName: string;
	sizeBytes: number;
	createdAt: string;
}

/** Mirrors `admin_template_core::backup::PendingRestoreInfo` (camelCase on the wire). */
export interface PendingRestoreInfo {
	sizeBytes: number;
	stagedAt: string;
}

/** Mirrors the Tauri `backups_open_folder` command's `OpenFolderResult` (`src-tauri/src/lib.rs`). */
export interface OpenFolderResult {
	opened: boolean;
	path: string;
}

export const DEMO_MODE_MESSAGE = 'デモモードでは利用できません';

function demoModeError(): ProviderError {
	return new ProviderError({ kind: 'other', message: DEMO_MODE_MESSAGE });
}

/** Is this environment backed by a real backup/restore service (Tauri or the embedded server)? False in plain-browser demo mode. */
export function isBackupsAvailable(): boolean {
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

function authHeaders(extra?: Record<string, string>): Record<string, string> {
	const headers: Record<string, string> = { ...CSRF_HEADER, ...extra };
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;
	return headers;
}

/** Parse a non-2xx REST response body into a `ProviderError`, same shape-guessing as usersAdmin.ts/auditLogAdmin.ts. */
async function errorFromResponse(response: Response): Promise<ProviderError> {
	let body: unknown;
	try {
		body = await response.json();
	} catch {
		return new ProviderError({
			kind: 'other',
			message: `${response.status} ${response.statusText}`
		});
	}
	if (isErrorBody(body)) return new ProviderError(body);
	return new ProviderError({ kind: 'other', message: `${response.status} ${response.statusText}` });
}

interface HttpJsonInit {
	method: string;
	body?: unknown;
	expectNoContent?: boolean;
}

async function httpJson<T>(path: string, init: HttpJsonInit): Promise<T> {
	const hasBody = init.body !== undefined;
	const headers = authHeaders(hasBody ? { 'Content-Type': 'application/json' } : undefined);

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

	if (!response.ok) throw await errorFromResponse(response);
	if (init.expectNoContent) return undefined as T;
	return (await response.json()) as T;
}

/** `admin`-only: create a new backup (`VACUUM INTO`). */
export async function createBackup(): Promise<BackupInfo> {
	if (!isBackupsAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<BackupInfo>('backups_create');
	return httpJson<BackupInfo>('/api/backups', { method: 'POST' });
}

/** `admin`-only: list existing backups, newest first. */
export async function listBackups(): Promise<BackupInfo[]> {
	if (!isBackupsAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<BackupInfo[]>('backups_list');
	return httpJson<BackupInfo[]>('/api/backups', { method: 'GET' });
}

/**
 * `admin`-only, REST/LAN mode only (spec M17: "LANブラウザ向けダウンロード
 * （REST）。デスクトップはフォルダを開く"): fetch the raw bytes of
 * `fileName` and save them via a temporary `Blob` object URL, same download
 * pattern as `routes/(app)/items/+page.svelte`'s `downloadTextFile`. In
 * Tauri mode this rejects with a message directing the caller to
 * `openBackupsFolder` instead - there is no browser download mechanism
 * inside the webview.
 */
export async function downloadBackup(fileName: string): Promise<void> {
	if (!isBackupsAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		throw new ProviderError({
			kind: 'other',
			message:
				'デスクトップアプリではダウンロードできません。「フォルダを開く」から直接コピーしてください'
		});
	}

	let response: Response;
	try {
		response = await fetch(`/api/backups/${encodeURIComponent(fileName)}`, {
			headers: authHeaders()
		});
	} catch {
		throw new ProviderError({ kind: 'other', message: NETWORK_ERROR_MESSAGE });
	}
	if (!response.ok) throw await errorFromResponse(response);

	const blob = await response.blob();
	const url = URL.createObjectURL(blob);
	try {
		const a = document.createElement('a');
		a.href = url;
		a.download = fileName;
		document.body.appendChild(a);
		a.click();
		document.body.removeChild(a);
	} finally {
		URL.revokeObjectURL(url);
	}
}

/** `admin`-only, Tauri only: open the `backups/` directory in the OS file explorer. */
export async function openBackupsFolder(): Promise<OpenFolderResult> {
	if (!isBackupsAvailable()) throw demoModeError();
	if (getBantoMode() !== 'tauri') {
		throw new ProviderError({
			kind: 'other',
			message: 'この操作はデスクトップアプリでのみ利用できます'
		});
	}
	return invokeCommand<OpenFolderResult>('backups_open_folder');
}

/** `admin`-only: stage a restore from an existing backup already listed by `listBackups()`. Applied at next startup (spec M17). */
export async function stageRestoreFromBackup(fileName: string): Promise<void> {
	if (!isBackupsAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('backups_stage_restore', { fileName });
		return;
	}
	await httpJson<void>(`/api/backups/${encodeURIComponent(fileName)}/restore`, {
		method: 'POST',
		expectNoContent: true
	});
}

/**
 * `admin`-only, REST/LAN mode only (spec M17: desktop restores are staged
 * from the existing `backups/` list, not an arbitrary local upload - see
 * `stageRestoreFromBackup`). Uploads `file`'s raw bytes to
 * `POST /api/backups/restore?fileName=`, which stages it the same way as an
 * existing-file restore (integrity + schema validation, applied at next
 * startup).
 */
export async function uploadAndStageRestore(file: File): Promise<void> {
	if (!isBackupsAvailable()) throw demoModeError();
	if (getBantoMode() !== 'server') {
		throw new ProviderError({
			kind: 'other',
			message: 'ファイルからのリストアはLANブラウザでのみ利用できます。一覧から選択してください'
		});
	}

	const bytes = await file.arrayBuffer();
	const headers = authHeaders({ 'Content-Type': 'application/octet-stream' });
	const query = new URLSearchParams({ fileName: file.name });

	let response: Response;
	try {
		response = await fetch(`/api/backups/restore?${query.toString()}`, {
			method: 'POST',
			headers,
			body: bytes
		});
	} catch {
		throw new ProviderError({ kind: 'other', message: NETWORK_ERROR_MESSAGE });
	}
	if (!response.ok) throw await errorFromResponse(response);
}

/** `admin`-only: the currently-staged restore, if any. */
export async function getPendingRestore(): Promise<PendingRestoreInfo | null> {
	if (!isBackupsAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri')
		return invokeCommand<PendingRestoreInfo | null>('backups_pending');
	return httpJson<PendingRestoreInfo | null>('/api/backups/pending-restore', { method: 'GET' });
}

/** `admin`-only: cancel a staged restore. */
export async function cancelPendingRestore(): Promise<void> {
	if (!isBackupsAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('backups_cancel_restore');
		return;
	}
	await httpJson<void>('/api/backups/pending-restore', { method: 'DELETE', expectNoContent: true });
}
