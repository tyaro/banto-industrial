/**
 * 設定の REST（`/api/settings/*`・`/api/host`）を呼ぶ小さな fetch の助け（#392 B2-2）。
 * `hubAdmin.ts` の `httpJson` と同じ形: CSRF のヘッダー（`X-Banto-Client`）と、
 * 保存しているトークンがあれば `Authorization: Bearer`。失敗は Banto の `ErrorBody`
 * （`{ kind, message }`）を `ProviderError` にし、読めない本文は `"<status> <text>"`、
 * 届かないときは「サーバーに接続できません」。`serverAdmin.ts`・`authAdmin.ts`・
 * `hostInfo.ts` の `server` モードの経路が使う。
 */
import { getAuthProvider, isProviderError, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER } from './environment';

export const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

const ERROR_KINDS = new Set([
	'not_found',
	'validation',
	'bad_request',
	'unauthorized',
	'forbidden',
	'storage',
	'other'
]);

/** Banto の `ErrorBody` の形か（`hubAdmin.ts` などと同じ判定）。 */
export function isErrorBody(value: unknown): value is ErrorBody {
	if (typeof value !== 'object' || value === null) return false;
	const kind = (value as { kind?: unknown }).kind;
	return typeof kind === 'string' && ERROR_KINDS.has(kind);
}

/** Tauri の reject（`{ kind, message }`）・例外を `ProviderError` にそろえる。 */
export function toProviderError(err: unknown): ProviderError {
	if (isProviderError(err)) return err;
	if (isErrorBody(err)) return new ProviderError(err);
	const message = err instanceof Error ? err.message : String(err);
	return new ProviderError({ kind: 'other', message });
}

function currentToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
}

async function errorFromResponse(response: Response): Promise<ProviderError> {
	let body: unknown;
	try {
		body = await response.json();
	} catch {
		body = undefined;
	}
	if (isErrorBody(body)) return new ProviderError(body);
	return new ProviderError({ kind: 'other', message: `${response.status} ${response.statusText}` });
}

export interface RestJsonInit {
	method: 'GET' | 'PUT' | 'POST';
	body?: unknown;
	/** トークンを付けない（`GET /api/host` は認証なしで読める）。 */
	anonymous?: boolean;
}

/** `path`（同じ origin）へ JSON の要求を送り、2xx の本文を返す。 */
export async function restJson<T>(path: string, init: RestJsonInit): Promise<T> {
	const headers: Record<string, string> = { ...CSRF_HEADER };
	if (init.body !== undefined) headers['Content-Type'] = 'application/json';
	if (!init.anonymous) {
		const token = currentToken();
		if (token) headers.Authorization = `Bearer ${token}`;
	}
	let response: Response;
	try {
		response = await fetch(path, {
			method: init.method,
			headers,
			body: init.body !== undefined ? JSON.stringify(init.body) : undefined
		});
	} catch {
		throw new ProviderError({ kind: 'other', message: NETWORK_ERROR_MESSAGE });
	}
	if (!response.ok) throw await errorFromResponse(response);
	return (await response.json()) as T;
}
