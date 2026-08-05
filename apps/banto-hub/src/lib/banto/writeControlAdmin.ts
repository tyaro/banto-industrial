/**
 * `admin` 限定の書き込み受付トグルクライアント（T2-4、設計 §6-6、新規作成）。
 * `apps/banto-hub/core/src/rest.rs` の `POST /api/write-control/enable|disable`
 * に対応する - `apiKeysAdmin.ts`/`auditLogAdmin.ts` と同じ httpRequest 雛形。
 *
 * 現在の状態自体（`writeEnabled`/`writeWasEnabledBeforeRestart` に相当する
 * 値）は `GET /api/v1/status`（`hubStatus.ts` の `StatusResponse`、
 * snake_case の `write_enabled`/`write_was_enabled_before_restart`）を見る -
 * このファイルは enable/disable の**書き込み**操作のみを提供する。
 */
import { getAuthProvider, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER } from './setup';

/** `POST /api/write-control/enable|disable` の応答。 */
export interface WriteControlStatusResponse {
	write_enabled: boolean;
	write_was_enabled_before_restart: boolean;
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

async function httpPost(path: string): Promise<WriteControlStatusResponse> {
	const headers: Record<string, string> = { ...CSRF_HEADER };
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;

	let response: Response;
	try {
		response = await fetch(path, { method: 'POST', headers });
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

	return (await response.json()) as WriteControlStatusResponse;
}

export async function enableWriteControl(): Promise<WriteControlStatusResponse> {
	return httpPost('/api/write-control/enable');
}

export async function disableWriteControl(): Promise<WriteControlStatusResponse> {
	return httpPost('/api/write-control/disable');
}
