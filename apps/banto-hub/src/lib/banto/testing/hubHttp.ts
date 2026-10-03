/**
 * テスト専用（アプリからは import しない）: banto-hub のガード・再確認を
 * **本物の** `@banto/admin-core`（`createHttpAuthProvider` と既定の
 * SessionController）の上で走らせるための偽のサーバー。
 *
 * - `sessionStorage` / `localStorage` をメモリ上のものに差し替える。
 * - `window` を `storage` イベントを配れる最小の偽物に差し替える（HTTP
 *   provider は作るときに `window.addEventListener('storage', ...)` で別タブの
 *   Remember me の変化を聞く。#257）。
 * - `fetch` を差し替え、`/api/commissioning/status`・`/api/auth/identity`・
 *   `/api/auth/status`・`/api/auth/grant/commissioning`・`/api/auth/logout` に
 *   答える（`commissioning.ts` の本物の HTTP ヘルパーも、provider の
 *   `resolve()`/`enterGrant()`/`status()` も同じ `fetch` を通る）。送った要求
 *   （パス・ヘッダ・signal）を記録する。試運転モードの偽サーバーは
 *   {@link commissioningHub}。
 * - 既定の controller を作り直し（`resetDefaultSessionController`）、新しい
 *   provider で `initBanto` する（テストごとに独立）。
 */
import { vi } from 'vitest';
import { createHttpAuthProvider, initBanto, type DataProvider } from '@banto/admin-core';
import { resetDefaultSessionController } from '../../../../node_modules/@banto/admin-core/src/sessionController.svelte';

export const TOKEN_KEY = 'banto.auth.token';

export class MemoryStorage {
	private map = new Map<string, string>();
	getItem(key: string): string | null {
		return this.map.get(key) ?? null;
	}
	setItem(key: string, value: string): void {
		this.map.set(key, value);
	}
	removeItem(key: string): void {
		this.map.delete(key);
	}
	clear(): void {
		this.map.clear();
	}
}

export function jsonResponse(status: number, body: unknown): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'content-type': 'application/json' }
	});
}

export interface SentRequest {
	path: string;
	headers: Record<string, string>;
	signal: AbortSignal | null | undefined;
}

export type Route = (request: SentRequest) => Promise<Response>;

export function installHub() {
	const session = new MemoryStorage();
	const local = new MemoryStorage();
	const storageListeners = new Set<(event: { key: string | null }) => void>();
	vi.stubGlobal('sessionStorage', session);
	vi.stubGlobal('localStorage', local);
	vi.stubGlobal('window', {
		addEventListener(type: string, listener: (event: { key: string | null }) => void) {
			if (type === 'storage') storageListeners.add(listener);
		},
		removeEventListener(type: string, listener: (event: { key: string | null }) => void) {
			if (type === 'storage') storageListeners.delete(listener);
		}
	});

	const sent: SentRequest[] = [];
	const routes = {
		/** `GET /api/commissioning/status`。既定はロックダウン済み。 */
		status: (async () => jsonResponse(200, { lockedDown: true })) as Route,
		/** `GET /api/auth/identity`。既定は「セッション無し」（`200 null`）。 */
		identity: (async () => jsonResponse(200, null)) as Route,
		/** `GET /api/auth/status`。既定は「初期化済み・試運転の grant は出せない」。 */
		authStatus: (async () =>
			jsonResponse(200, { initialized: true, grants: { commissioning: false } })) as Route,
		/** `POST /api/auth/grant/commissioning`。既定は `403`（ロックダウン済み／loopback でない）。 */
		grant: (async () => jsonResponse(403, { kind: 'forbidden' })) as Route,
		/** `POST /api/auth/logout`。 */
		logout: (async () => jsonResponse(200, { success: true })) as Route
	};
	const fetchFn = vi.fn(async (url: string | URL | Request, init?: RequestInit) => {
		const request: SentRequest = {
			path: String(url),
			headers: { ...((init?.headers as Record<string, string> | undefined) ?? {}) },
			signal: init?.signal
		};
		sent.push(request);
		if (request.path.endsWith('/api/commissioning/status')) return routes.status(request);
		if (request.path.endsWith('/api/auth/identity')) return routes.identity(request);
		if (request.path.endsWith('/api/auth/status')) return routes.authStatus(request);
		if (request.path.endsWith('/api/auth/grant/commissioning')) return routes.grant(request);
		if (request.path.endsWith('/api/auth/logout')) return routes.logout(request);
		return jsonResponse(404, { kind: 'not_found', message: request.path });
	});
	vi.stubGlobal('fetch', fetchFn);

	resetDefaultSessionController();
	const auth = createHttpAuthProvider({ fetchFn: fetchFn as unknown as typeof fetch });
	initBanto({ dataProvider: {} as DataProvider, authProvider: auth, resources: [] });

	return {
		auth,
		session,
		local,
		sent,
		routes,
		fetchFn,
		paths(): string[] {
			return sent.map((r) => r.path);
		},
		/** 別タブが Remember me のトークンを書き換えた（`localStorage` + `storage` イベント）。 */
		otherTabWrites(token: string | null): void {
			if (token === null) local.removeItem(TOKEN_KEY);
			else local.setItem(TOKEN_KEY, token);
			for (const listener of [...storageListeners]) listener({ key: TOKEN_KEY });
		}
	};
}

export const identityOf = (id: string, role = 'admin', kind?: string) => ({
	id,
	name: id,
	role,
	...(kind === undefined ? {} : { kind })
});

/** 送られた Bearer トークンで答えを分ける `/api/auth/identity`。表に無いトークンは `401`。 */
export function identityByToken(table: Record<string, ReturnType<typeof identityOf>>): Route {
	return async (request) => {
		const token = request.headers.Authorization?.replace(/^Bearer /, '') ?? '';
		const identity = table[token];
		return identity ? jsonResponse(200, identity) : jsonResponse(401, { kind: 'unauthorized' });
	};
}

/** 試運転の grant のトークンに対する identity（サーバーの `kind: 'commissioning'`）。 */
export const COMMISSIONING_GRANT_IDENTITY = {
	id: 'commissioning',
	name: '試運転モード',
	role: 'admin',
	kind: 'commissioning'
};

/**
 * 試運転モード（未ロックダウン・loopback）の偽サーバー: `status` が
 * `grants.commissioning: true`、`grant` が `{ success: true, token }`、
 * `identity` は grant のトークンだけを試運転の identity にし、他のトークンは `401`。
 * `extra` でアカウントのトークンなど、ほかのトークンの答えを足せる。
 */
export function commissioningHub(
	hub: ReturnType<typeof installHub>,
	token = 'grant-token',
	extra: Record<string, ReturnType<typeof identityOf>> = {}
): void {
	hub.routes.authStatus = async () =>
		jsonResponse(200, { initialized: true, grants: { commissioning: true } });
	hub.routes.grant = async () => jsonResponse(200, { success: true, token });
	hub.routes.identity = identityByToken({ ...extra, [token]: COMMISSIONING_GRANT_IDENTITY });
}
