/**
 * `admin` 限定の pending changes 管理クライアント。
 * `apps/banto-hub/core/src/rest.rs` の
 * `GET /api/pending-changes` / `POST /api/pending-changes/{id}/apply|cancel`
 * に対応する。`tagRegistryAdmin.ts` と同じ HTTP 専用パターンで、wire は
 * バックエンドの `PendingChange` / 409 conflict 応答に合わせて camelCase を使う。
 */
import { getAuthProvider, ProviderError, type ErrorBody } from '@banto/admin-core';
import { mapLiveReconfigureFailure } from './liveReconfigure';
import { CSRF_HEADER } from './setup';

export type PendingChangeState = 'pending' | 'applying' | 'applied' | 'canceled' | 'failed';

/** Mirrors `banto_hub_core::pending_changes::PendingChange` (`#[serde(rename_all = "camelCase")]`). */
export interface PendingChange {
	id: number;
	state: PendingChangeState;
	source: string;
	payload: unknown;
	baseConfiguredRevision: number;
	createdAt: string;
	updatedAt: string;
	requestedByUsername: string | null;
	requestedByRole: string | null;
	failureReason: string | null;
}

// #341（2026-09-14 オーナー回答「明示適用も無停止」）: ここには 409
// `collection_edit_locked`（収集稼働中の適用拒否）の応答型
// `PendingApplyConflict` / `PendingApplyConflictError` と、その 409 を
// 判別する `isPendingApplyConflictBody` / `mapApplyConflict` /
// `isPendingApplyConflictError` があったが、サーバー側がその 409 を返さなく
// なったので丸ごと撤去した（適用は収集を止めずに通る）。適用時に残る 409 は
// per-resource フィンガープリント不一致（`pending_apply_conflict`）だけで、
// これは従来どおり汎用エラーとして表示する。

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
	mapErrorBody?: (body: unknown, status: number) => Error | undefined;
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
		const mapped = init.mapErrorBody?.(body, response.status);
		if (mapped) throw mapped;
		if (isErrorBody(body)) throw new ProviderError(body);
		throw new ProviderError({
			kind: 'other',
			message: `${response.status} ${response.statusText}`
		});
	}

	return (await response.json()) as T;
}

export async function listPendingChanges(limit = 100): Promise<PendingChange[]> {
	const safeLimit = Math.max(1, Math.min(1000, Math.trunc(limit)));
	return httpRequest<PendingChange[]>(`/api/pending-changes?limit=${safeLimit}`, { method: 'GET' });
}

export async function applyPendingChange(id: number): Promise<PendingChange> {
	return httpRequest<PendingChange>(`/api/pending-changes/${id}/apply`, {
		method: 'POST',
		mapErrorBody: mapLiveReconfigureFailure
	});
}

export async function cancelPendingChange(id: number): Promise<PendingChange> {
	return httpRequest<PendingChange>(`/api/pending-changes/${id}/cancel`, {
		method: 'POST'
	});
}

export async function requeuePendingChange(id: number): Promise<PendingChange> {
	return httpRequest<PendingChange>(`/api/pending-changes/${id}/requeue`, {
		method: 'POST'
	});
}
