/** `admin` 限定の OPC UA サーバー設定クライアント（#201）。 */
import { getAuthProvider, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER } from './setup';

export interface OpcUaSettings {
	enabled: boolean;
	bind: string;
	port: number;
	endpointPath: string;
	namespaceUri: string;
	advertisedHost: string | null;
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

function currentToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
}

async function request(method: 'GET' | 'PUT', body?: OpcUaSettings): Promise<OpcUaSettings> {
	const headers: Record<string, string> = { ...CSRF_HEADER };
	if (body) headers['Content-Type'] = 'application/json';
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;

	let response: Response;
	try {
		response = await fetch('/api/opcua-settings', {
			method,
			headers,
			body: body ? JSON.stringify(body) : undefined
		});
	} catch {
		throw new ProviderError({ kind: 'other', message: 'サーバーに接続できません' });
	}

	if (!response.ok) {
		let error: unknown;
		try {
			error = await response.json();
		} catch {
			throw new ProviderError({
				kind: 'other',
				message: `${response.status} ${response.statusText}`
			});
		}
		if (isErrorBody(error)) throw new ProviderError(error);
		throw new ProviderError({
			kind: 'other',
			message: `${response.status} ${response.statusText}`
		});
	}

	return (await response.json()) as OpcUaSettings;
}

export function getOpcUaSettings(): Promise<OpcUaSettings> {
	return request('GET');
}

/** 保存した設定はサーバーが即時適用する。 */
export function saveOpcUaSettings(input: OpcUaSettings): Promise<OpcUaSettings> {
	return request('PUT', input);
}
