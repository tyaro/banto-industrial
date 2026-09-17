/**
 * Client for the #383 段階2a / R1-B タグレジストリ API: `plc_connections`
 * (PLC接続), `collection_groups` (収集グループ) と `tags`（タグ）- banto-tags
 * の3階層レジストリを、このアプリが REST/Tauri 両経路で公開したもの。
 * `usersAdmin.ts` と同じ三環境分岐:
 *
 * - Tauri webview -> `invoke()` の `plc_connections_*` / `collection_groups_*`
 *   / `tags_*` コマンド（`apps/chronogazer/src-tauri/src/lib.rs`）。
 * - LAN ブラウザ（組み込みサーバー） -> `fetch()` の
 *   `/api/plc-connections[...]` / `/api/collection-groups[...]` /
 *   `/api/tags[...]` REST ルート（`apps/chronogazer/core/src/rest.rs`）。
 * - 単体ブラウザのデモモード（`vite dev`/`vite preview`）-> レジストリDBが
 *   無いので全呼び出しが `DEMO_MODE_MESSAGE` で拒否される。
 *   `isTagRegistryAvailable()` で事前に判定できる。
 *
 * 3エンティティとも editor-write / viewer-read（R0 §3.6）で、REST/Tauri
 * 両経路が同じ内容で監査する。型は `chronogazer_core::rest` の camelCase
 * `*Payload`/response 型と1:1（banto-tags 自身の snake_case `*Input` では
 * ない - payload がワイヤ形状を持つ）。
 *
 * relay-wright の同名ファイル（`apps/relay-wright/src/lib/banto/
 * tagRegistryAdmin.ts`）を手本にしつつ、chronogazer には無いカスケード削除
 * （relay-wright の feature/easy-delete、このアプリのスコープ外）は持ち込ま
 * ない。banto-hub の 1029 行版（バッチ・式チェック等）も同様に移植しない。
 */
import { invoke } from '@tauri-apps/api/core';
import { getAuthProvider, isProviderError, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER, getBantoMode } from './setup';

// --- wire types (camelCase, matching the Rust serde shapes) -----------------

/**
 * chronogazer は PLC 直結アプリで、`banto_tags::ALLOWED_PROTOCOLS` の
 * `"virtual"`/`"postgres"`（どちらも banto-hub 固有）には対応しない -
 * `chronogazer_core::rest::reject_disallowed_connection_protocol`の doc
 * comment参照。
 */
export type PlcProtocol = 'modbus-tcp' | 'slmp';

/** `""` = 未指定（プロトコルの既定に従う）。 */
export type WordOrder = '' | 'low_high' | 'high_low';

/** Mirrors `chronogazer_core::rest::PlcConnectionResponse`. */
export interface PlcConnection {
	id: number;
	name: string;
	protocol: PlcProtocol;
	host: string;
	port: number;
	unitId: number;
	enabled: boolean;
	wordOrder: WordOrder;
}

/** Mirrors `chronogazer_core::rest::PlcConnectionPayload`. */
export interface PlcConnectionInput {
	name: string;
	protocol: PlcProtocol;
	host: string;
	port: number;
	unitId: number;
	enabled: boolean;
	wordOrder: WordOrder;
}

/** Mirrors `banto_tags::CollectionGroup`. */
export interface CollectionGroup {
	id: number;
	name: string;
	plcConnectionId: number;
	periodMs: number;
	enabled: boolean;
}

/** Mirrors `chronogazer_core::rest::CollectionGroupPayload`. */
export interface CollectionGroupInput {
	name: string;
	plcConnectionId: number;
	periodMs: number;
	enabled: boolean;
}

/**
 * Selectable collection periods (ms) - mirrors `banto_tags::ALLOWED_PERIOD_MS`
 * (recorder-requirements.md §3.1)。バックエンドはこれ以外を人間可読な検証
 * エラーで拒否するので、画面はこの選択肢しか出さない。
 */
export const ALLOWED_PERIOD_MS: readonly number[] = [100, 200, 500, 1000, 2000, 5000, 10000, 60000];

/**
 * R0 §3.1 が要求するデータ型（ビット/16bit/32bit 符号有無/実数、および
 * #325 の64bit拡張）。`banto_tags::ALLOWED_DATA_TYPES` のうち `"string"` は
 * 対応する `stringLength`/`stringEncoding` の入力導線が無いため、このPRの
 * ペイロード/画面からは除いている（`TagInput` doc comment参照）。
 */
export type TagDataType = 'bit' | 'i16' | 'u16' | 'i32' | 'u32' | 'f32' | 'i64' | 'u64' | 'f64';

/** Mirrors `banto_tags::Tag`（本PRが公開するフィールドのみ）。 */
export interface Tag {
	id: number;
	name: string;
	collectionGroupId: number;
	address: string;
	dataType: TagDataType;
	rawLo: number | null;
	rawHi: number | null;
	engLo: number | null;
	engHi: number | null;
	unit: string | null;
	decimals: number;
	enabled: boolean;
}

/** Mirrors `chronogazer_core::rest::TagPayload`. */
export interface TagInput {
	name: string;
	collectionGroupId: number;
	address: string;
	dataType: TagDataType;
	rawLo?: number | null;
	rawHi?: number | null;
	engLo?: number | null;
	engHi?: number | null;
	unit?: string | null;
	decimals: number;
	enabled: boolean;
}

// --- environment/error plumbing (usersAdmin.ts と同じ作法) ------------------

export const DEMO_MODE_MESSAGE = 'デモモードでは利用できません';
const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

function demoModeError(): ProviderError {
	return new ProviderError({ kind: 'other', message: DEMO_MODE_MESSAGE });
}

/** レジストリDBを持つ環境（Tauri または組み込みサーバー）か？ デモモードなら false。 */
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
