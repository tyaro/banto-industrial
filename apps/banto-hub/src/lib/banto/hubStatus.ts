/**
 * タグ空間 API（`/api/v1/*`）のうち状態モニタ画面が使う2ルートのクライアント
 * （`GET /api/v1/status`・`GET /api/v1/values`、新規作成）。
 *
 * **注意: `/api/v1/*` の応答は snake_case**（`apps/banto-hub/core/src/rest.rs`
 * の `StatusResponse`/`ValuesResponse`/`ValueEntry`/`ConnectionStatusEntry`
 * 参照）。`usersAdmin.ts`/`tagRegistryAdmin.ts` 等の管理系 camelCase DTO と
 * 混同しないこと - このファイルの型は意図的に snake_case のまま宣言している。
 *
 * 認証はセッション bearer（`Authorization: Bearer <token>`）のみで足りる。
 * `/api/v1/*` は CSRF ヘッダ（`X-Banto-Client`）を要求しない設計
 * （設計 §5.1/§5.6: 機械クライアント向けで任意ヘッダを付けられる前提の
 * ため CSRF の脅威モデルに乗らない）なので、ここでは付けていない。
 */
import { getAuthProvider, ProviderError, type ErrorBody } from '@banto/admin-core';

/** `GET /api/v1/status` の `connections` 配列1件分。 */
export interface ConnectionStatusEntry {
	name: string;
	id: number;
	status: 'connected' | 'reconnecting' | 'stopped';
	attempt: number | null;
}

/** `GET /api/v1/status` の `mqtt`（T3、設計 §5.3）。 */
export interface MqttStatusEntry {
	enabled: boolean;
	connected: boolean;
}

/**
 * `GET /api/v1/status` の `grpc`（T4、設計 §5.4）。MQTT と違い「実際に
 * 接続できているか」のライブ状態は持たない - gRPC サーバーは listen する
 * だけなので、設定値がそのまま意図した状態を表す。
 */
export interface GrpcStatusEntry {
	enabled: boolean;
	port: number;
}

/** `GET /api/v1/status` の応答。 */
export interface StatusResponse {
	version: string;
	revision: number;
	last_config_error: string | null;
	connections: ConnectionStatusEntry[];
	/** T2-4（設計 §6-6）: 書き込み受付のライブフラグ（起動時は必ず false）。 */
	write_enabled: boolean;
	/** T2-4（設計 §6-6）: プロセス再起動前は有効だったか（表示専用の履歴）。 */
	write_was_enabled_before_restart: boolean;
	/** T3（設計 §5.3）: MQTT publish の設定/接続状態。 */
	mqtt: MqttStatusEntry;
	/** T4（設計 §5.4）: gRPC サーバーの設定。 */
	grpc: GrpcStatusEntry;
	/** T14-4 の収集ライフサイクル状態 - mirrors `CollectionState::as_str()`
	 * (`apps/banto-hub/core/src/controller.rs`). `GET /api/v1/status` の
	 * `collection_state` フィールド。 */
	collection_state: 'stopped' | 'starting' | 'running' | 'stopping' | 'faulted' | string;
}

/** `GET /api/v1/values` の1タグ分（`{ tag, v, q, t }`）。 */
export interface ValueEntry {
	tag: string;
	v: number | null;
	q: 'good' | 'bad' | 'stale' | string;
	t: number;
}

/** `GET /api/v1/values` の応答。 */
export interface ValuesResponse {
	revision: number;
	t: number;
	values: ValueEntry[];
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

async function httpGet<T>(path: string): Promise<T> {
	const headers: Record<string, string> = {};
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;

	let response: Response;
	try {
		response = await fetch(path, { method: 'GET', headers });
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

export async function getHubStatus(): Promise<StatusResponse> {
	return httpGet<StatusResponse>('/api/v1/status');
}

/** 全タグの現在値スナップショット（`?tags=` 省略 = 全件）。 */
export async function getHubValues(): Promise<ValuesResponse> {
	return httpGet<ValuesResponse>('/api/v1/values');
}
