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

/**
 * listener の状態（#634、`GET /api/opcua-status`）。設定の保存（PUT）は起動の
 * 完了を待たずに返るので、起動中・起動済み・失敗（理由つき）・停止はここで見る。
 */
export type OpcUaState = 'stopped' | 'starting' | 'running' | 'failed';

export interface OpcUaStatus {
	state: OpcUaState;
	/** 起動済みのときだけ、クライアントへ案内する endpoint の URL。 */
	endpoint: string | null;
	/** 失敗のときだけ、その理由。 */
	error: string | null;
	/** この状態を作った設定の世代。保存のたびに進む。 */
	generation: number;
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

async function request<T>(
	method: 'GET' | 'PUT',
	path: '/api/opcua-settings' | '/api/opcua-status',
	body?: OpcUaSettings,
	signal?: AbortSignal
): Promise<T> {
	const headers: Record<string, string> = { ...CSRF_HEADER };
	if (body) headers['Content-Type'] = 'application/json';
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;

	let response: Response;
	try {
		response = await fetch(path, {
			method,
			headers,
			body: body ? JSON.stringify(body) : undefined,
			signal
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

	try {
		return (await response.json()) as T;
	} catch {
		// 本文の読み取りも `signal` で打ち切られる。
		throw new ProviderError({ kind: 'other', message: 'サーバーに接続できません' });
	}
}

export function getOpcUaSettings(): Promise<OpcUaSettings> {
	return request<OpcUaSettings>('GET', '/api/opcua-settings');
}

/**
 * 設定を保存する。サーバーは保存して listener の停止・起動を始めたらすぐ返る
 * （#634）。起動の結果は {@link getOpcUaStatus} で見る。
 */
export function saveOpcUaSettings(input: OpcUaSettings): Promise<OpcUaSettings> {
	return request<OpcUaSettings>('PUT', '/api/opcua-settings', input);
}

/** `signal` で打ち切れる（画面は期限つきで呼ぶ。`opcuaStatusPoller.ts`）。 */
export function getOpcUaStatus(signal?: AbortSignal): Promise<OpcUaStatus> {
	return request<OpcUaStatus>('GET', '/api/opcua-status', undefined, signal);
}
