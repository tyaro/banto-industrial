/**
 * `admin` 限定の MQTT publish 設定クライアント（T3、設計 §5.3、新規作成）。
 * `apps/banto-hub/core/src/rest.rs` の `GET/PUT /api/mqtt-settings` に対応する
 * - `apiKeysAdmin.ts`/`usersAdmin.ts` と同じ httpRequest 雛形（camelCase の
 * admin-UI リソース）。
 *
 * `password` は `GET` の応答に一切含まれない（サーバー側が型そのものに
 * フィールドを持たせていない - `MqttSettingsResponse`のdoc comment参照）。
 * `PUT` の `password` は空文字を送ると「変更なし」として扱われる（設計 §5.3
 * 実装指示どおり）- フォームは「パスワードを変更する場合のみ入力」という
 * UI にする（`+page.svelte`参照）。
 *
 * 保存(`PUT`)は即時適用される（設計実装指示「保存で即時適用」）- 現在の
 * 接続状態（`connected`）は `GET /api/v1/status`（`hubStatus.ts`）の
 * `mqtt.connected` を見る。このファイルは設定の読み書きのみを提供する。
 */
import { getAuthProvider, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER } from './setup';

/** `GET/PUT /api/mqtt-settings`の応答（`password`フィールドは無い）。 */
export interface MqttSettings {
	enabled: boolean;
	host: string;
	port: number;
	clientId: string;
	username: string | null;
	prefix: string;
	qos: number;
	minIntervalMs: number;
}

/**
 * `PUT /api/mqtt-settings`の入力。`password`は空文字/未指定で「変更なし」
 * （このファイルの doc comment参照）。
 */
export interface MqttSettingsInput {
	enabled: boolean;
	host: string;
	port: number;
	clientId: string;
	username: string | null;
	password: string;
	prefix: string;
	qos: number;
	minIntervalMs: number;
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

export async function getMqttSettings(): Promise<MqttSettings> {
	return httpRequest<MqttSettings>('/api/mqtt-settings', { method: 'GET' });
}

/** 保存 - 成功すると即時適用される（このファイルの doc comment参照）。 */
export async function saveMqttSettings(input: MqttSettingsInput): Promise<MqttSettings> {
	return httpRequest<MqttSettings>('/api/mqtt-settings', { method: 'PUT', body: input });
}
