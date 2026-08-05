/**
 * `admin` 限定のユーザー管理 API クライアント。chronogazer の同名ファイル
 * から複製し、Tauri 分岐・デモモード分岐を削除して HTTP 一択にした
 * （banto-hub は headless axum サーバーのみで Tauri を持たない - `setup.ts`
 * 参照）。ワイヤ形状（camelCase）は `apps/banto-hub/core/src/rest.rs` の
 * `UserIdentityResponse`/`UserSummary` と一致する。
 */
import { getAuthProvider, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER } from './setup';

export type Role = 'admin' | 'editor' | 'viewer';

/** Mirrors `banto_hub_core::users::UserSummary`（wire は camelCase）。 */
export interface UserSummary {
	id: number;
	username: string;
	displayName: string;
	role: Role;
	createdAt: string;
}

/** Mirrors REST の `UserIdentityResponse`。 */
export interface CreatedUser {
	id: number;
	username: string;
	displayName: string;
	role: Role;
}

export interface CreateUserInput {
	username: string;
	password: string;
	displayName: string;
	role: Role;
}

export interface UpdateUserInput {
	displayName: string;
	role: Role;
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

/** usersAdmin.ts と同じトークン取得（アクティブな AuthProvider が createHttpAuthProvider の結果である前提）。 */
function currentToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
}

interface HttpInit {
	method: string;
	body?: unknown;
	/** `reset-password` は `200 {success}`、`delete` は `204` - 呼び出し側がボディ不要な場合に指定。 */
	expectNoContent?: boolean;
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

	if (init.expectNoContent) return undefined as T;
	return (await response.json()) as T;
}

export async function listUsers(): Promise<UserSummary[]> {
	return httpRequest<UserSummary[]>('/api/users', { method: 'GET' });
}

export async function createUser(input: CreateUserInput): Promise<CreatedUser> {
	return httpRequest<CreatedUser>('/api/users', { method: 'POST', body: input });
}

export async function updateUser(id: number, input: UpdateUserInput): Promise<UserSummary> {
	return httpRequest<UserSummary>(`/api/users/${id}`, { method: 'PUT', body: input });
}

export async function resetUserPassword(id: number, newPassword: string): Promise<void> {
	// REST は `200 {success}` を返す（204ではない）が、非2xxでない限りこの
	// クライアントが必要とする情報はそれで揃っている。
	await httpRequest<{ success: boolean }>(`/api/users/${id}/reset-password`, {
		method: 'POST',
		body: { newPassword }
	});
}

export async function deleteUser(id: number): Promise<void> {
	await httpRequest<void>(`/api/users/${id}`, { method: 'DELETE', expectNoContent: true });
}
