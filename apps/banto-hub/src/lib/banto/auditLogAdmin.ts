/**
 * `admin` 限定の監査ログ閲覧 API クライアント。chronogazer の同名ファイル
 * から複製し、Tauri 分岐・デモモード分岐を削除して HTTP 一択にした。
 *
 * `getAuditConfig`/`setAuditConfig`（`GET`/`PUT /api/audit-log/config`）は
 * **削除した** — banto-hub バックエンドにはその保持ポリシー設定ルートが
 * 存在しない（`apps/banto-hub/core/src/rest.rs` は `/api/audit-log/list`
 * のみを公開している。実装指示: 「getAuditConfig/setAuditConfig は削除
 * （バックエンドに /api/audit-log/config が無い）」）。
 */
import {
	getAuthProvider,
	ProviderError,
	type ErrorBody,
	type ListParams,
	type ListResult
} from '@banto/admin-core';
import { CSRF_HEADER } from './setup';

/** Mirrors `banto_hub_core::audit::AuditLogEntry`（wire は camelCase）。 */
export interface AuditLogEntry {
	id: number;
	ts: string;
	actorUsername: string | null;
	actorRole: string | null;
	action: string;
	resource: string;
	entityId: string | null;
	/** 保存されている生の JSON 文字列。表示時に必要に応じて JSON.parse する。 */
	detail: string | null;
	origin: string;
	result: string;
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

/** フィルタ/ソート/ページングつきの監査ログ読み取り（admin限定の閲覧画面用）。 */
export async function listAuditLog(params: ListParams): Promise<ListResult<AuditLogEntry>> {
	return httpRequest<ListResult<AuditLogEntry>>('/api/audit-log/list', {
		method: 'POST',
		body: params
	});
}
