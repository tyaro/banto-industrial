/**
 * I1 タグレジストリ API（`plc_connections`/`collection_groups`/`tags` -
 * banto-tags の三層レジストリ）のクライアント。
 *
 * relay-wright の同名ファイル（336行）から複製し、Tauri 分岐を削除して
 * HTTP 一択にした。**型定義・ワイヤ形状は relay-wright から1バイトも
 * 変えていない** — banto-hub の `/api/plc-connections|collection-groups|tags`
 * REST は relay-wright/chronogazer と完全に同型（camelCase の
 * `*Payload`/`*Input` DTO、`apps/banto-hub/core/src/rest.rs` の
 * `PlcConnectionPayload`/`CollectionGroupPayload`/`TagPayload` 参照）。
 *
 * relay-wright にあった「プレーン `vite dev`/デモモードでは
 * DEMO_MODE_MESSAGE で reject する」という三環境分岐は完全に削除した —
 * banto-hub にはそもそもデモモードが存在しない（`setup.ts` が常に HTTP
 * プロバイダを配線する）ため、`isTagRegistryAvailable()`/
 * `DEMO_MODE_MESSAGE` のような「利用不可」表現自体が不要と判断し、
 * ページ側の呼び出し元ごと削っている。
 */
import { getAuthProvider, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER } from './setup';

// --- wire types (camelCase, matching the Rust serde shapes) -----------------

export type PlcProtocol = 'modbus-tcp' | 'slmp' | 'virtual';

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

/**
 * T6-2 (docs/tag-server-design.md §4.2/§4.3(a)): the two reserved
 * `protocol: "virtual"` connections `banto-hub` auto-provisions at startup
 * (`bin/banto-hub.rs::ensure_virtual_connection`) — `calc` hosts every
 * `tagKind: "computed"` tag's group, `mem` hosts every `"internal"` tag's
 * group. Mirrors `banto_tags::{CALC_CONNECTION_NAME, MEM_CONNECTION_NAME}`.
 * The backend also refuses to edit/delete a `"virtual"` connection
 * (`PlcConnectionService::update`/`delete`) — the plc-connections page uses
 * these names to show that row as read-only rather than letting the user
 * discover the 403 by trying.
 */
export const CALC_CONNECTION_NAME = 'calc';
export const MEM_CONNECTION_NAME = 'mem';

/** True for the two reserved auto-provisioned connections (see above). */
export function isVirtualConnection(conn: Pick<PlcConnection, 'protocol'>): boolean {
	return conn.protocol === 'virtual';
}

/** Mirrors `banto_hub_core::rest::PlcConnectionPayload`. */
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

/** Mirrors `banto_hub_core::rest::CollectionGroupPayload`. */
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

/**
 * Tag species (T6-2, docs/tag-server-design.md §4.2's table) — mirrors
 * `banto_tags::ALLOWED_TAG_KINDS`.
 *
 * - `plc` (default/existing): value from the collection task, `address`
 *   required, `expression` forbidden.
 * - `computed`: value from evaluating `expression` (banto-expr); `address`
 *   forbidden, `expression` required, `writable` forced false server-side
 *   (write attempts always 403 — "値は式が決める").
 * - `internal`: value from client writes, held entirely in the tag space
 *   (never sent to a PLC); `address`/`expression` both forbidden, `retain`
 *   selects restart persistence.
 *
 * Placement is enforced server-side (`banto_tags::tag::validate_tag_kind_placement`):
 * `computed` only under the `calc` connection, `internal` only under `mem`,
 * `plc` under neither.
 */
export type TagKind = 'plc' | 'computed' | 'internal';

export const TAG_KIND_OPTIONS: { value: TagKind; label: string }[] = [
	{ value: 'plc', label: 'plc（PLC 収集）' },
	{ value: 'computed', label: 'computed（演算タグ）' },
	{ value: 'internal', label: 'internal（内部タグ）' }
];

/** Mirrors `banto_tags::Tag`. */
export interface Tag {
	id: number;
	name: string;
	collectionGroupId: number;
	address: string;
	dataType: TagDataType;
	/**
	 * Consecutive 16-bit word devices a `string` tag occupies; `Some(1..=128)`
	 * iff `dataType === 'string'`, `null` otherwise.
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
	/** Per-tag write opt-in (T2-3, docs/tag-server-design.md §6 item 1). */
	writable: boolean;
	/** T6-2: tag species — see {@link TagKind}. */
	tagKind: TagKind;
	/** T6-2: computed-tag formula source, only set when `tagKind === 'computed'`. */
	expression: string | null;
	/** T6-2: internal-tag restart-persistence flag. */
	retain: boolean;
}

/** Mirrors `banto_hub_core::rest::TagPayload`. */
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
	/** T2-3: `#[serde(default)]` on the backend - omitting this still creates
	 * a non-writable tag, so existing callers of `createTag`/`updateTag` are
	 * unaffected. The admin UI (this app's tags page) always sends it
	 * explicitly from its new checkbox. */
	writable?: boolean;
	/** T6-2: `#[serde(default)]` (= `"plc"`) on the backend. */
	tagKind?: TagKind;
	/** T6-2: required when `tagKind === 'computed'`, otherwise omitted. */
	expression?: string | null;
	/** T6-2: `#[serde(default)]` (= `false`) on the backend. */
	retain?: boolean;
}

/** `string` tag `stringLength` bounds — mirrors the backend CHECK/validation. */
export const MIN_STRING_LENGTH = 1;
export const MAX_STRING_LENGTH = 128;

// --- error plumbing (mirrors writeRegistryAdmin.ts) --------------------------

const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

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
	return httpRequest<PlcConnection[]>('/api/plc-connections', { method: 'GET' });
}

export async function createPlcConnection(input: PlcConnectionInput): Promise<PlcConnection> {
	return httpRequest<PlcConnection>('/api/plc-connections', { method: 'POST', body: input });
}

export async function updatePlcConnection(
	id: number,
	input: PlcConnectionInput
): Promise<PlcConnection> {
	return httpRequest<PlcConnection>(`/api/plc-connections/${id}`, { method: 'PUT', body: input });
}

export async function deletePlcConnection(id: number): Promise<void> {
	await httpRequest<void>(`/api/plc-connections/${id}`, {
		method: 'DELETE',
		expectNoContent: true
	});
}

// --- collection groups ------------------------------------------------------

export async function listCollectionGroups(): Promise<CollectionGroup[]> {
	return httpRequest<CollectionGroup[]>('/api/collection-groups', { method: 'GET' });
}

export async function createCollectionGroup(input: CollectionGroupInput): Promise<CollectionGroup> {
	return httpRequest<CollectionGroup>('/api/collection-groups', { method: 'POST', body: input });
}

export async function updateCollectionGroup(
	id: number,
	input: CollectionGroupInput
): Promise<CollectionGroup> {
	return httpRequest<CollectionGroup>(`/api/collection-groups/${id}`, {
		method: 'PUT',
		body: input
	});
}

export async function deleteCollectionGroup(id: number): Promise<void> {
	await httpRequest<void>(`/api/collection-groups/${id}`, {
		method: 'DELETE',
		expectNoContent: true
	});
}

// --- tags -------------------------------------------------------------------

export async function listTags(): Promise<Tag[]> {
	return httpRequest<Tag[]>('/api/tags', { method: 'GET' });
}

export async function createTag(input: TagInput): Promise<Tag> {
	return httpRequest<Tag>('/api/tags', { method: 'POST', body: input });
}

export async function updateTag(id: number, input: TagInput): Promise<Tag> {
	return httpRequest<Tag>(`/api/tags/${id}`, { method: 'PUT', body: input });
}

export async function deleteTag(id: number): Promise<void> {
	await httpRequest<void>(`/api/tags/${id}`, { method: 'DELETE', expectNoContent: true });
}
