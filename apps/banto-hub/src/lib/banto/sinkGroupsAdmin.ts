/**
 * S6（docs/banto-hub-external-db-design.md §5.2・§7 row S6）: DB Sink の
 * `hub_sink_groups` CRUD クライアント（`GET/POST /api/sink/groups`、
 * `GET/PUT/DELETE /api/sink/groups/{id}` - `apps/banto-hub/core/src/rest.rs`
 * の `sink_groups_router`）。`apiKeysAdmin.ts` と同じ httpRequest 雛形を流用
 * した HTTP 専用クライアント。
 *
 * S4（PR #305）で Hub 側の REST・サービス層は完了済み - このファイルは
 * その wire 形（`SinkGroup`/`SinkGroupInput`、camelCase）をそのまま写した
 * だけで、サーバー側の検証ルールを増減させていない。`sink_groups_create`/
 * `sink_groups_update` は稼働中でも即時反映（設計 §6-13「pending queue には
 * 載せない」）で 202 応答は返らない - `isQueuedWhileRunningError` のような
 * 分岐は不要（他の PLC 系エンティティと違う点、`tagRegistryAdmin.ts` との
 * 差異）。
 */
import { getAuthProvider, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER } from './setup';

/** `hub_sink_groups.mode` - mirrors `banto_hub_core::sink::ALLOWED_SINK_MODES`。 */
export type SinkGroupMode = 'interval' | 'on_change';

export const ALLOWED_SINK_MODES: readonly SinkGroupMode[] = ['interval', 'on_change'];

/** mirrors `banto_hub_core::sink::service`（`MIN_INTERVAL_MS`/`MAX_INTERVAL_MS`）。 */
export const MIN_SINK_INTERVAL_MS = 100;
export const MAX_SINK_INTERVAL_MS = 3_600_000;

/** mirrors `banto_hub_core::sink::service::MAX_NAME_LEN`。 */
export const MAX_SINK_GROUP_NAME_LEN = 64;

/** Mirrors `banto_hub_core::sink::SinkGroup`（`GET/POST/PUT /api/sink/groups*` 応答）。 */
export interface SinkGroup {
	id: number;
	name: string;
	dbConnectionId: number;
	mode: SinkGroupMode;
	intervalMs: number;
	tableName: string;
	storeBad: boolean;
	enabled: boolean;
	tagIds: number[];
}

/** Mirrors `banto_hub_core::sink::SinkGroupInput`（create/update の共通 payload）。 */
export interface SinkGroupInput {
	name: string;
	dbConnectionId: number;
	mode: SinkGroupMode;
	intervalMs: number;
	tableName: string;
	storeBad: boolean;
	enabled: boolean;
	tagIds: number[];
}

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
	/** `tagRegistryAdmin.ts::deletePlcConnection`と同じ規約 - DELETE の 204 応答を JSON パースしない。 */
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

export async function listSinkGroups(): Promise<SinkGroup[]> {
	return httpRequest<SinkGroup[]>('/api/sink/groups', { method: 'GET' });
}

export async function createSinkGroup(input: SinkGroupInput): Promise<SinkGroup> {
	return httpRequest<SinkGroup>('/api/sink/groups', { method: 'POST', body: input });
}

export async function updateSinkGroup(id: number, input: SinkGroupInput): Promise<SinkGroup> {
	return httpRequest<SinkGroup>(`/api/sink/groups/${id}`, { method: 'PUT', body: input });
}

export async function deleteSinkGroup(id: number): Promise<void> {
	await httpRequest<void>(`/api/sink/groups/${id}`, { method: 'DELETE', expectNoContent: true });
}
