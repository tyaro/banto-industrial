/**
 * `admin` 限定の pending changes 管理クライアント。
 * `apps/banto-hub/core/src/rest.rs` の
 * `GET /api/pending-changes` / `POST /api/pending-changes/{id}/apply|cancel`
 * に対応する。`tagRegistryAdmin.ts` と同じ HTTP 専用パターンで、wire は
 * バックエンドの `PendingChange` / 409 conflict 応答に合わせて camelCase を使う。
 */
import { getAuthProvider, ProviderError, type ErrorBody } from '@banto/admin-core';
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

/** 409 `collection_edit_locked` の応答本体。 */
export interface PendingApplyConflict {
	error: 'collection_edit_locked';
	state: unknown;
	status: unknown;
	message: string;
	failureReason?: string;
	pending?: PendingChange;
}

export class PendingApplyConflictError extends Error implements PendingApplyConflict {
	readonly error = 'collection_edit_locked';
	readonly state: unknown;
	readonly status: unknown;
	readonly failureReason?: string;
	readonly pending?: PendingChange;

	constructor(body: PendingApplyConflict) {
		super(body.message);
		this.name = 'PendingApplyConflictError';
		this.state = body.state;
		this.status = body.status;
		this.failureReason = body.failureReason;
		this.pending = body.pending;
	}
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

function isPendingChange(value: unknown): value is PendingChange {
	if (typeof value !== 'object' || value === null) return false;
	const candidate = value as {
		id?: unknown;
		state?: unknown;
		source?: unknown;
		createdAt?: unknown;
	};
	return (
		typeof candidate.id === 'number' &&
		typeof candidate.state === 'string' &&
		typeof candidate.source === 'string' &&
		typeof candidate.createdAt === 'string'
	);
}

function isPendingApplyConflictBody(value: unknown): value is PendingApplyConflict {
	if (typeof value !== 'object' || value === null) return false;
	const candidate = value as {
		error?: unknown;
		state?: unknown;
		status?: unknown;
		message?: unknown;
		failureReason?: unknown;
		pending?: unknown;
	};
	if (candidate.error !== 'collection_edit_locked' || typeof candidate.message !== 'string') {
		return false;
	}
	if (
		candidate.failureReason !== undefined &&
		candidate.failureReason !== null &&
		typeof candidate.failureReason !== 'string'
	) {
		return false;
	}
	if (
		candidate.pending !== undefined &&
		candidate.pending !== null &&
		!isPendingChange(candidate.pending)
	) {
		return false;
	}
	return true;
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

function mapApplyConflict(body: unknown, status: number): Error | undefined {
	if (status !== 409 || !isPendingApplyConflictBody(body)) return undefined;
	return new PendingApplyConflictError(body);
}

export function isPendingApplyConflictError(error: unknown): error is PendingApplyConflictError {
	return error instanceof PendingApplyConflictError;
}

export async function listPendingChanges(limit = 100): Promise<PendingChange[]> {
	const safeLimit = Math.max(1, Math.min(1000, Math.trunc(limit)));
	return httpRequest<PendingChange[]>(`/api/pending-changes?limit=${safeLimit}`, { method: 'GET' });
}

export async function applyPendingChange(id: number): Promise<PendingChange> {
	return httpRequest<PendingChange>(`/api/pending-changes/${id}/apply`, {
		method: 'POST',
		mapErrorBody: mapApplyConflict
	});
}

export async function cancelPendingChange(id: number): Promise<PendingChange> {
	return httpRequest<PendingChange>(`/api/pending-changes/${id}/cancel`, {
		method: 'POST'
	});
}
