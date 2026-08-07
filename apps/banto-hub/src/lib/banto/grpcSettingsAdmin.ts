/**
 * `admin` 限定の gRPC サーバー設定クライアント（T4、設計 §5.4、新規作成）。
 * `apps/banto-hub/core/src/rest.rs` の `GET/PUT /api/grpc-settings` に対応する
 * - `mqttSettingsAdmin.ts`/`apiKeysAdmin.ts` と同じ httpRequest 雛形（camelCase
 * の admin-UI リソース）。
 *
 * MQTT と違い認証情報（パスワード等）を持たないため、`MqttSettingsInput`の
 * ような「変更なし」を表す空文字規約は不要 - `enabled`/`port`/`bind` は
 * 常に現在値をそのまま読み書きする（この UI からの保存では常にフォームに
 * 表示中の値を送る - 既存の port 入力と同じ扱い）。
 *
 * `bind`（2026-08-08 オーナー決定、docs/improvement-plan.md H3）: 既定は
 * `127.0.0.1`。サーバー側の `PUT` は `bind` を省略（`undefined`/`null`）
 * すると現在値を維持する規約を持つが、この管理 UI は常に現在値を積んで
 * 送るため実質使わない（プログラムからの直接呼び出しでポートだけを
 * 変えたい場合等に使える経路として、型は `bind` を必須のままにしている
 * - サーバー側の「省略可」は `+page.svelte` の外側の API 利用者向け）。
 *
 * 保存(`PUT`)は即時適用される（設計実装指示「保存で即時適用」）- サーバー側が
 * 保存直後に `crate::grpc::GrpcServer::apply` を呼ぶ。
 */
import { getAuthProvider, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER } from './setup';

/** `GET/PUT /api/grpc-settings`の応答/入力（同一形）。 */
export interface GrpcSettings {
	enabled: boolean;
	/**
	 * bind アドレス（既定 `127.0.0.1`）。`127.0.0.1` はこの PC のみ、
	 * `0.0.0.0` は全インターフェースに listen する（`+page.svelte`の
	 * 説明文参照）。
	 */
	bind: string;
	port: number;
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

export async function getGrpcSettings(): Promise<GrpcSettings> {
	return httpRequest<GrpcSettings>('/api/grpc-settings', { method: 'GET' });
}

/** 保存 - 成功すると即時適用される（このファイルの doc comment参照）。 */
export async function saveGrpcSettings(input: GrpcSettings): Promise<GrpcSettings> {
	return httpRequest<GrpcSettings>('/api/grpc-settings', { method: 'PUT', body: input });
}
