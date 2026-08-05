/**
 * Client for the R1-B tag-registry API: `plc_connections` (PLC接続),
 * `collection_groups` (収集グループ) and `tags` (ソースタグ) — banto-tags'
 * three-tier registry, wired dual-path by this app. Same three-environment
 * split as `writeRegistryAdmin.ts`:
 *
 * - Tauri webview -> `invoke()` the `plc_connections_*` /
 *   `collection_groups_*` / `tags_*` commands
 *   (`apps/relay-wright/src-tauri/src/lib.rs`).
 * - LAN browser served by the embedded server -> `fetch()` the
 *   `/api/plc-connections[...]` / `/api/collection-groups[...]` /
 *   `/api/tags[...]` REST routes (`apps/relay-wright/core/src/rest.rs`).
 * - Plain `vite dev`/`vite preview` demo -> no backend/registry DB, so every
 *   call rejects with `DEMO_MODE_MESSAGE`; `isTagRegistryAvailable()` lets a
 *   page show the note up front.
 *
 * All three entities are editor-write / viewer-read on the backend (spec
 * M10), audited symmetrically on both paths (invariant §1 両経路対称). The
 * input types mirror `relay_wright_core::rest`'s camelCase `*Payload` DTOs
 * (NOT banto-tags' snake_case `*Input` structs — the payloads own the wire
 * shape for both paths).
 */
import { invoke } from '@tauri-apps/api/core';
import { getAuthProvider, isProviderError, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER, getBantoMode } from './setup';

// --- wire types (camelCase, matching the Rust serde shapes) -----------------

export type PlcProtocol = 'modbus-tcp' | 'slmp';

/** Mirrors `banto_tags::PlcConnection`. */
export interface PlcConnection {
	id: number;
	name: string;
	protocol: PlcProtocol;
	host: string;
	port: number;
	unitId: number;
	enabled: boolean;
}

/** Mirrors `relay_wright_core::rest::PlcConnectionPayload`. */
export interface PlcConnectionInput {
	name: string;
	protocol: PlcProtocol;
	host: string;
	port: number;
	unitId: number;
	enabled: boolean;
}

/** Mirrors `banto_tags::CollectionGroup`. */
export interface CollectionGroup {
	id: number;
	name: string;
	plcConnectionId: number;
	periodMs: number;
	enabled: boolean;
}

/** Mirrors `relay_wright_core::rest::CollectionGroupPayload`. */
export interface CollectionGroupInput {
	name: string;
	plcConnectionId: number;
	periodMs: number;
	enabled: boolean;
}

/**
 * Selectable collection periods (ms) — mirrors
 * `banto_tags::ALLOWED_PERIOD_MS` (the backend rejects anything else with a
 * field-level validation error, so the UI only ever offers these).
 */
export const ALLOWED_PERIOD_MS: readonly number[] = [100, 200, 500, 1000, 2000, 5000, 10000, 60000];

export type TagDataType = 'bit' | 'i16' | 'u16' | 'i32' | 'u32' | 'f32' | 'string';

/** Mirrors `banto_tags::Tag`. */
export interface Tag {
	id: number;
	name: string;
	collectionGroupId: number;
	address: string;
	dataType: TagDataType;
	/**
	 * Consecutive 16-bit word devices a `string` tag occupies (SJIS capacity =
	 * 2 bytes/word); `Some(1..=128)` iff `dataType === 'string'`, `null`
	 * otherwise (S2 文字列タグ).
	 */
	stringLength: number | null;
	rawLo: number | null;
	rawHi: number | null;
	engLo: number | null;
	engHi: number | null;
	unit: string | null;
	decimals: number;
	thresholdH: number | null;
	thresholdHh: number | null;
	thresholdL: number | null;
	thresholdLl: number | null;
	enabled: boolean;
}

/** Mirrors `relay_wright_core::rest::TagPayload`. */
export interface TagInput {
	name: string;
	collectionGroupId: number;
	address: string;
	dataType: TagDataType;
	stringLength?: number | null;
	rawLo?: number | null;
	rawHi?: number | null;
	engLo?: number | null;
	engHi?: number | null;
	unit?: string | null;
	decimals: number;
	thresholdH?: number | null;
	thresholdHh?: number | null;
	thresholdL?: number | null;
	thresholdLl?: number | null;
	enabled: boolean;
}

/** `string` tag `stringLength` bounds — mirrors the backend CHECK/validation. */
export const MIN_STRING_LENGTH = 1;
export const MAX_STRING_LENGTH = 128;

// --- environment/error plumbing (mirrors writeRegistryAdmin.ts) -------------

export const DEMO_MODE_MESSAGE = 'デモモードでは利用できません';
const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

function demoModeError(): ProviderError {
	return new ProviderError({ kind: 'other', message: DEMO_MODE_MESSAGE });
}

/** Backed by a real registry DB (Tauri or embedded server)? False in demo mode. */
export function isTagRegistryAvailable(): boolean {
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

// --- PLC connections --------------------------------------------------------

export async function listPlcConnections(): Promise<PlcConnection[]> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<PlcConnection[]>('plc_connections_list');
	return httpRequest<PlcConnection[]>('/api/plc-connections', { method: 'GET' });
}

export async function createPlcConnection(input: PlcConnectionInput): Promise<PlcConnection> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<PlcConnection>('plc_connections_create', { input });
	}
	return httpRequest<PlcConnection>('/api/plc-connections', { method: 'POST', body: input });
}

export async function updatePlcConnection(
	id: number,
	input: PlcConnectionInput
): Promise<PlcConnection> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<PlcConnection>('plc_connections_update', { id, input });
	}
	return httpRequest<PlcConnection>(`/api/plc-connections/${id}`, { method: 'PUT', body: input });
}

export async function deletePlcConnection(id: number): Promise<void> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('plc_connections_delete', { id });
		return;
	}
	await httpRequest<void>(`/api/plc-connections/${id}`, {
		method: 'DELETE',
		expectNoContent: true
	});
}

// --- cascade delete (feature/easy-delete) -----------------------------------
// banto-tags' plain deletes refuse while children exist; these wire the
// relay-wright-side cascade (`registry_cascade` in relay-wright-core):
// preview returns the would-be counts (shown in the confirm dialog,
// including write-side references that will be left dangling), the cascade
// deletes tags → groups → connection in one transaction and returns what it
// removed.

/** Mirrors `relay_wright_core::registry_cascade::ConnectionCascadePreview`. */
export interface PlcConnectionCascadePreview {
	/** Collection groups that will be deleted with the connection. */
	groups: number;
	/** Tags that will be deleted with those groups. */
	tags: number;
	/** Write targets on this connection - left dangling (warned, not deleted). */
	writeTargets: number;
	/** Write rules referencing the doomed tags - left dangling (warned, not deleted). */
	writeRules: number;
}

/** Mirrors `relay_wright_core::registry_cascade::ConnectionCascadeSummary`. */
export interface PlcConnectionCascadeSummary {
	groups: number;
	tags: number;
}

/** Mirrors `relay_wright_core::registry_cascade::GroupCascadePreview`. */
export interface CollectionGroupCascadePreview {
	tags: number;
	writeRules: number;
}

/** Mirrors `relay_wright_core::registry_cascade::GroupCascadeSummary`. */
export interface CollectionGroupCascadeSummary {
	tags: number;
}

export async function previewPlcConnectionCascade(
	id: number
): Promise<PlcConnectionCascadePreview> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<PlcConnectionCascadePreview>('plc_connections_cascade_preview', { id });
	}
	return httpRequest<PlcConnectionCascadePreview>(`/api/plc-connections/${id}/cascade-preview`, {
		method: 'GET'
	});
}

export async function cascadeDeletePlcConnection(id: number): Promise<PlcConnectionCascadeSummary> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<PlcConnectionCascadeSummary>('plc_connections_cascade_delete', { id });
	}
	return httpRequest<PlcConnectionCascadeSummary>(`/api/plc-connections/${id}/cascade`, {
		method: 'DELETE'
	});
}

export async function previewCollectionGroupCascade(
	id: number
): Promise<CollectionGroupCascadePreview> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<CollectionGroupCascadePreview>('collection_groups_cascade_preview', {
			id
		});
	}
	return httpRequest<CollectionGroupCascadePreview>(
		`/api/collection-groups/${id}/cascade-preview`,
		{ method: 'GET' }
	);
}

export async function cascadeDeleteCollectionGroup(
	id: number
): Promise<CollectionGroupCascadeSummary> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<CollectionGroupCascadeSummary>('collection_groups_cascade_delete', {
			id
		});
	}
	return httpRequest<CollectionGroupCascadeSummary>(`/api/collection-groups/${id}/cascade`, {
		method: 'DELETE'
	});
}

// --- collection groups ------------------------------------------------------

export async function listCollectionGroups(): Promise<CollectionGroup[]> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<CollectionGroup[]>('collection_groups_list');
	}
	return httpRequest<CollectionGroup[]>('/api/collection-groups', { method: 'GET' });
}

export async function createCollectionGroup(input: CollectionGroupInput): Promise<CollectionGroup> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<CollectionGroup>('collection_groups_create', { input });
	}
	return httpRequest<CollectionGroup>('/api/collection-groups', { method: 'POST', body: input });
}

export async function updateCollectionGroup(
	id: number,
	input: CollectionGroupInput
): Promise<CollectionGroup> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<CollectionGroup>('collection_groups_update', { id, input });
	}
	return httpRequest<CollectionGroup>(`/api/collection-groups/${id}`, {
		method: 'PUT',
		body: input
	});
}

export async function deleteCollectionGroup(id: number): Promise<void> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('collection_groups_delete', { id });
		return;
	}
	await httpRequest<void>(`/api/collection-groups/${id}`, {
		method: 'DELETE',
		expectNoContent: true
	});
}

// --- tags -------------------------------------------------------------------

export async function listTags(): Promise<Tag[]> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<Tag[]>('tags_list');
	return httpRequest<Tag[]>('/api/tags', { method: 'GET' });
}

export async function createTag(input: TagInput): Promise<Tag> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<Tag>('tags_create', { input });
	}
	return httpRequest<Tag>('/api/tags', { method: 'POST', body: input });
}

export async function updateTag(id: number, input: TagInput): Promise<Tag> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<Tag>('tags_update', { id, input });
	}
	return httpRequest<Tag>(`/api/tags/${id}`, { method: 'PUT', body: input });
}

export async function deleteTag(id: number): Promise<void> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('tags_delete', { id });
		return;
	}
	await httpRequest<void>(`/api/tags/${id}`, { method: 'DELETE', expectNoContent: true });
}
