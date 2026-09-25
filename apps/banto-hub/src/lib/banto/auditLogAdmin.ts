/**
 * `admin` 限定の監査ログ閲覧 API クライアント。chronogazer の同名ファイル
 * から複製し、Tauri 分岐・デモモード分岐を削除して HTTP 一択にした。
 *
 * `getAuditConfig`/`setAuditConfig`（`GET`/`PUT /api/audit-log/config`）は
 * このファイルには**持っていない**。複製した当時は banto-hub にそのルートが
 * 無かったため削除した。その後 P3-a（docs/banto-hub-remaining-plan.md）で
 * `apps/banto-hub/core/src/rest.rs` に admin 限定の `GET`/`PUT
 * /api/audit-log/config` が足されたが、この画面のクライアントには戻して
 * いない（保持ポリシーの表示・変更の UI が banto-hub にはまだ無い）。
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
	signal?: AbortSignal;
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
			body: hasBody ? JSON.stringify(init.body) : undefined,
			signal: init.signal
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

/**
 * Mirrors `banto_hub_core::audit::AuditLogList`（#428）: `ListResult` の綴り
 * そのままに、**この応答が使ったスナップショット境界** `asOfId` を足したもの。
 */
export interface AuditLogList extends ListResult<AuditLogEntry> {
	asOfId: number;
}

/**
 * 監査ログ 1 ブロックの読み取りに掛ける上限（#428。chronogazer の #410 と
 * 同じ値）。backend はローカルの SQLite に対する索引つきの `DELETE`（保持
 * 期間）と `SELECT` だけなので、これに当たるのは「遅い」ではなく「返って
 * こない」。上限が無いと飛行中のブロックが `loading` を抱えたまま降りず、
 * 「再読み込み」が押せなくなる。
 */
export const AUDIT_LIST_TIMEOUT_MS = 15000;

/**
 * フィルタ/ソート/ページングつきの監査ログ読み取り（admin限定の閲覧画面用）。
 *
 * **`asOfId` = スナップショット境界**（#428）。`null` を渡すとサーバーが
 * **その時点の最大 `id`** を境界にして、使った境界を [`AuditLogList.asOfId`]
 * で返す。画面は 1 つの「世代」の最初の応答でそれを固定し、同じ世代の後続
 * ブロックにすべて渡す（判断は `routes/(app)/audit-log/auditBlocks.ts`）。
 */
export async function listAuditLog(
	params: ListParams,
	asOfId: number | null = null,
	signal?: AbortSignal
): Promise<AuditLogList> {
	const path =
		asOfId === null ? '/api/audit-log/list' : `/api/audit-log/list?asOfId=${String(asOfId)}`;
	return httpRequest<AuditLogList>(path, { method: 'POST', body: params, signal });
}
