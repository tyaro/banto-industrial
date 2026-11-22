/**
 * Client for the `admin`-only user-management API (spec M10 RBAC). Same
 * split as serverAdmin.ts: Tauri webview calls `invoke()` directly (the
 * `users_*` commands, `apps/admin-template/src-tauri/src/lib.rs`), a LAN
 * browser client served by the embedded server calls `fetch()` against
 * `/api/users/*` (`apps/admin-template/core/src/rest.rs`), reusing the same
 * bearer-token/CSRF-header mechanism `@banto/admin-core`'s
 * `createHttpDataProvider`/`createHttpAuthProvider` use.
 *
 * Deliberately NOT built on `@banto/admin-core`'s generic
 * `DataProvider`/`getDataProvider()` (unlike items): the wire shapes here
 * don't fit that resource-CRUD convention -
 * `users_create`/`users_update`/`users_reset_password` take individual named
 * arguments rather than a `values` bag, `users_list` takes no `ListParams`,
 * and the REST routes are plain `/api/users[...]`, not
 * `/api/{resource}/list`. A small dedicated client mirroring
 * providers/tauri.ts + providers/http.ts's error handling is simpler and
 * clearer than bending the generic contract to fit.
 */
import { invoke } from '@tauri-apps/api/core';
import { getAuthProvider, isProviderError, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER, getBantoMode } from './setup';

export type Role = 'admin' | 'editor' | 'viewer';

/** Mirrors `admin_template_core::users::UserSummary` (camelCase on the wire). */
export interface UserSummary {
	id: number;
	username: string;
	displayName: string;
	role: Role;
	createdAt: string;
}

/** Mirrors `users_create`'s Tauri `UserIdentityResult` / REST `UserIdentityResponse` - no `createdAt` (see their doc comments). */
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

const ERROR_KINDS = new Set([
	'not_found',
	'validation',
	'unauthorized',
	'forbidden',
	'conflict',
	'storage',
	'other'
]);

/** Same type guard as providers/tauri.ts / providers/http.ts (spec §10/§11.1). */
function isErrorBody(value: unknown): value is ErrorBody {
	if (typeof value !== 'object' || value === null) return false;
	const kind = (value as { kind?: unknown }).kind;
	return typeof kind === 'string' && ERROR_KINDS.has(kind);
}

function toProviderError(err: unknown): ProviderError {
	if (isProviderError(err)) return err;
	if (isErrorBody(err)) return new ProviderError(err);
	const message = err instanceof Error ? err.message : String(err);
	return new ProviderError({ kind: 'other', message });
}

async function invokeCommand<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	try {
		return (await invoke(cmd, args)) as T;
	} catch (err) {
		throw toProviderError(err);
	}
}

const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

interface HttpInit {
	method: string;
	body?: unknown;
	/** `reset-password` returns `200 {success}`, `delete` returns `204` - both callers that don't need the body pass this. */
	expectNoContent?: boolean;
}

/** Bearer token shared with whichever `AuthProvider` is active - only `createHttpAuthProvider`'s result exposes `getToken()` (spec §11.1); other providers have none, which is fine (`getBantoMode() === 'server'` is only true when it's the http provider). */
function currentToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
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
	if (getBantoMode() === 'tauri') return invokeCommand<UserSummary[]>('users_list');
	return httpRequest<UserSummary[]>('/api/users', { method: 'GET' });
}

export async function createUser(input: CreateUserInput): Promise<CreatedUser> {
	if (getBantoMode() === 'tauri') {
		return invokeCommand<CreatedUser>('users_create', {
			username: input.username,
			password: input.password,
			displayName: input.displayName,
			role: input.role
		});
	}
	return httpRequest<CreatedUser>('/api/users', { method: 'POST', body: input });
}

export async function updateUser(id: number, input: UpdateUserInput): Promise<UserSummary> {
	if (getBantoMode() === 'tauri') {
		return invokeCommand<UserSummary>('users_update', {
			id,
			displayName: input.displayName,
			role: input.role
		});
	}
	return httpRequest<UserSummary>(`/api/users/${id}`, { method: 'PUT', body: input });
}

export async function resetUserPassword(id: number, newPassword: string): Promise<void> {
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('users_reset_password', { id, newPassword });
		return;
	}
	// REST responds `200 {success}` (rest.rs's ResetPasswordResponse), not
	// 204 - but the body carries no information this client needs beyond
	// "the request succeeded", which a non-2xx status already rules out.
	await httpRequest<{ success: boolean }>(`/api/users/${id}/reset-password`, {
		method: 'POST',
		body: { newPassword }
	});
}

export async function deleteUser(id: number): Promise<void> {
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('users_delete', { id });
		return;
	}
	await httpRequest<void>(`/api/users/${id}`, { method: 'DELETE', expectNoContent: true });
}
