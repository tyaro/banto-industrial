/**
 * `admin` 限定の API キー管理クライアント（新規作成、T0-2 の
 * `apps/banto-hub/core/src/api_keys.rs`/`rest.rs` の `/api/api-keys/*` に
 * 対応）。`usersAdmin.ts`/`tagRegistryAdmin.ts` と同じ httpRequest 雛形を
 * 流用した HTTP 専用クライアント（banto-hub に Tauri/デモモードは無い）。
 *
 * `POST /api/api-keys` の応答に含まれる平文 `key` は **この応答限り**
 * （`apps/banto-hub/core/src/api_keys.rs` の doc comment 参照 - DB には
 * ハッシュしか保存されない）。DELETE ルートは存在しない（失効履歴を残す
 * 設計のため revoke のみ、設計 §5.6）。
 */
import { getAuthProvider, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER } from './setup';

/** Mirrors `banto_hub_core::api_keys::ApiKeySummary`（wire は camelCase）。 */
export interface ApiKeySummary {
	id: number;
	name: string;
	prefix: string;
	scopes: string[];
	createdAt: string;
	lastUsedAt: number | null;
	revokedAt: string | null;
}

export interface CreateApiKeyInput {
	name: string;
	scopes: string[];
}

/** Mirrors REST の `IssuedApiKeyResponse` - `key` はこの応答でしか手に入らない平文全体。 */
export interface IssuedApiKey {
	id: number;
	name: string;
	prefix: string;
	scopes: string[];
	key: string;
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

export async function listApiKeys(): Promise<ApiKeySummary[]> {
	return httpRequest<ApiKeySummary[]>('/api/api-keys', { method: 'GET' });
}

/** 発行 - 応答の `key` はこの呼び出し限りでしか手に入らない（呼び出し側は必ずその場で表示すること）。 */
export async function createApiKey(input: CreateApiKeyInput): Promise<IssuedApiKey> {
	return httpRequest<IssuedApiKey>('/api/api-keys', { method: 'POST', body: input });
}

/** 失効（冪等）。DELETE は無い - 失効履歴は revokedAt として残る。 */
export async function revokeApiKey(id: number): Promise<ApiKeySummary> {
	return httpRequest<ApiKeySummary>(`/api/api-keys/${id}/revoke`, { method: 'POST' });
}
