/**
 * `admin` 限定の書き込み監査閲覧クライアント（T2-4、設計 §6-3、新規作成）。
 * `apps/banto-hub/core/src/write_audit.rs`/`rest.rs` の
 * `POST /api/write-audit/list` に対応する - `auditLogAdmin.ts` とほぼ同型
 * （フィルタ/ソート/ページングつきの POST 1本）。
 */
import {
	getAuthProvider,
	ProviderError,
	type ErrorBody,
	type ListParams,
	type ListResult
} from '@banto/admin-core';
import { CSRF_HEADER } from './setup';

/** Mirrors `banto_hub_core::write_audit::WriteAuditEntry`（wire は camelCase）。 */
export interface WriteAuditEntry {
	id: number;
	ts: string;
	apiKeyId: number;
	apiKeyNameSnapshot: string;
	tagId: number;
	externalNameSnapshot: string;
	valueRequested: number | null;
	action: 'write' | 'rate_limit_tripped' | string;
	result: 'ok' | 'failed' | 'suppressed_disabled' | 'suppressed_rate_limited' | string;
	detail: string | null;
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

async function httpRequest<T>(path: string, body: unknown): Promise<T> {
	const headers: Record<string, string> = { ...CSRF_HEADER, 'Content-Type': 'application/json' };
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;

	let response: Response;
	try {
		response = await fetch(path, { method: 'POST', headers, body: JSON.stringify(body) });
	} catch {
		throw new ProviderError({ kind: 'other', message: NETWORK_ERROR_MESSAGE });
	}

	if (!response.ok) {
		let parsed: unknown;
		try {
			parsed = await response.json();
		} catch {
			throw new ProviderError({
				kind: 'other',
				message: `${response.status} ${response.statusText}`
			});
		}
		if (isErrorBody(parsed)) throw new ProviderError(parsed);
		throw new ProviderError({
			kind: 'other',
			message: `${response.status} ${response.statusText}`
		});
	}

	return (await response.json()) as T;
}

/** フィルタ/ソート/ページングつきの書き込み監査読み取り（admin限定の閲覧画面用）。 */
export async function listWriteAudit(params: ListParams): Promise<ListResult<WriteAuditEntry>> {
	return httpRequest<ListResult<WriteAuditEntry>>('/api/write-audit/list', params);
}
