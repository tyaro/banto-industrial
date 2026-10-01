/**
 * (app) ルートガード（`routes/(app)/+layout.ts`）と `sessionGuard.ts` の
 * ユニットテスト（banto v1.7.0 #204、v2.0.0 #260 で書き直し）。
 *
 * 守りたいこと: サーバーがセッションを**照合できなかった**とき（照合の
 * `500`・到達不能）、ルートガードは
 * - 503（エラー画面と再試行）にし、/login へ送らない
 * - 保存しているトークン（通常の `sessionStorage` も Remember me の
 *   `localStorage` も）を消さない
 * - `status()` / `enterPublicViewer()`（閲覧者への切り替え）を呼ばない
 *   （ChronoGazer には閲覧公開が無い）
 *
 * v1 は `resolveProtectedSession` を包んだ `decideProtectedRoute` を直接
 * テストしていたが、v2 で確定は SessionController の役目になったので、
 * **本物の** `@banto/admin-core` の HTTP 認証プロバイダーと既定の controller
 * （`initBanto`）の上で、ガードの `load()` そのものを呼ぶ。`bantoReady` と
 * テーマ設定の同期だけを差し替える。
 *
 * 後半は、デスクトップ（Tauri の `auth_resolve` が DB エラーで reject する）と、
 * 起動時の「組み込みサーバーか」の判定（`isBantoAuthCheckResponse`）。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { isHttpError, isRedirect } from '@sveltejs/kit';
import {
	createHttpAuthProvider,
	getSessionController,
	initBanto,
	type AuthProvider,
	type CredentialRevision,
	type DataProvider
} from '@banto/admin-core';

vi.mock('$lib/banto/setup', () => ({ bantoReady: Promise.resolve() }));
vi.mock('$lib/settings.svelte', () => ({ settings: { syncFromProvider: async () => {} } }));

import { load } from '../../routes/(app)/+layout';
import { isBantoAuthCheckResponse, SESSION_CHECK_FAILED_MESSAGE } from './sessionGuard';

const TOKEN_KEY = 'banto.auth.token';

class MemoryStorage {
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

let local: MemoryStorage;
let session: MemoryStorage;

beforeEach(() => {
	local = new MemoryStorage();
	session = new MemoryStorage();
	vi.stubGlobal('localStorage', local);
	vi.stubGlobal('sessionStorage', session);
});

afterEach(() => {
	vi.unstubAllGlobals();
});

function jsonResponse(status: number, body: unknown): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'content-type': 'application/json' }
	});
}

function useProvider(authProvider: AuthProvider): void {
	initBanto({ dataProvider: {} as DataProvider, authProvider, resources: [] });
}

/** A real HTTP provider whose `fetch` answers `/api/auth/identity` with `identity`, recording every path asked for. */
function httpProviderWith(identity: () => Promise<Response>) {
	const paths: string[] = [];
	const fetchFn = vi.fn(async (url: string | URL | Request) => {
		const path = String(url);
		paths.push(path);
		if (path.endsWith('/api/auth/identity')) return identity();
		return jsonResponse(200, { initialized: true, viewerPublic: true });
	}) as unknown as typeof fetch;
	useProvider(createHttpAuthProvider({ fetchFn }));
	return { paths };
}

/** Run the guard; return what it threw (or its data). */
async function runGuard(): Promise<
	| { kind: 'data'; sessionGeneration: number }
	| { kind: 'redirect'; location: string }
	| { kind: 'error'; status: number; message: string }
> {
	try {
		const data = await load();
		return { kind: 'data', sessionGeneration: data.sessionGeneration };
	} catch (thrown) {
		if (isRedirect(thrown)) return { kind: 'redirect', location: thrown.location };
		if (isHttpError(thrown)) {
			return { kind: 'error', status: thrown.status, message: thrown.body.message };
		}
		throw thrown;
	}
}

const unverifiedCases: [string, () => Promise<Response>][] = [
	[
		'照合の 500（DB が答えない）',
		async () => jsonResponse(500, { kind: 'storage', message: 'database is locked' })
	],
	[
		'到達不能（fetch が失敗）',
		async () => {
			throw new TypeError('Failed to fetch');
		}
	]
];

describe('(app) ガード: 照合できないときはトークンを残してエラー画面（503）', () => {
	for (const [label, identity] of unverifiedCases) {
		it(`${label}: 通常のセッション`, async () => {
			session.setItem(TOKEN_KEY, 'normal-token');
			const { paths } = httpProviderWith(identity);

			expect(await runGuard()).toEqual({
				kind: 'error',
				status: 503,
				message: SESSION_CHECK_FAILED_MESSAGE
			});
			expect(session.getItem(TOKEN_KEY)).toBe('normal-token');
			// 閲覧者への切り替え（status → enterPublicViewer）もしない
			expect(paths.every((path) => path.endsWith('/api/auth/identity'))).toBe(true);
		});

		it(`${label}: Remember me のセッション`, async () => {
			local.setItem(TOKEN_KEY, 'remembered-token');
			const { paths } = httpProviderWith(identity);

			expect(await runGuard()).toMatchObject({ kind: 'error', status: 503 });
			expect(local.getItem(TOKEN_KEY)).toBe('remembered-token');
			expect(paths.every((path) => path.endsWith('/api/auth/identity'))).toBe(true);
		});
	}
});

describe('(app) ガード: 確定したときだけ判断する', () => {
	it('401（失効）はログイン画面へ。トークンは消える。閲覧者への切り替えはしない', async () => {
		local.setItem(TOKEN_KEY, 'revoked-token');
		const { paths } = httpProviderWith(async () => jsonResponse(401, { kind: 'unauthorized' }));

		expect(await runGuard()).toEqual({ kind: 'redirect', location: '/login' });
		expect(local.getItem(TOKEN_KEY)).toBeNull();
		expect(paths.every((path) => path.endsWith('/api/auth/identity'))).toBe(true);
	});

	it('トークンが無ければ問い合わせずにログイン画面へ', async () => {
		const { paths } = httpProviderWith(async () => jsonResponse(200, null));
		expect(await runGuard()).toEqual({ kind: 'redirect', location: '/login' });
		expect(paths).toEqual([]);
	});

	it('確定した identity はそのまま進み、確定した世代を返す（ストアは snapshot から読む）', async () => {
		session.setItem(TOKEN_KEY, 'live-token');
		httpProviderWith(async () => jsonResponse(200, { id: 'alice', name: 'Alice', role: 'admin' }));

		const result = await runGuard();
		const snapshot = getSessionController().snapshot;
		expect(snapshot).toMatchObject({ status: 'active', owner: 'account:alice' });
		expect(result).toEqual({ kind: 'data', sessionGeneration: snapshot.generation });
		expect(session.getItem(TOKEN_KEY)).toBe('live-token');
	});
});

describe('(app) ガード: Tauri の auth_resolve が DB エラーで reject したとき', () => {
	/** A standard provider (as the Tauri one) whose `resolve()` is `answer`. */
	function tauriLike(answer: AuthProvider['resolve']) {
		const status = vi.fn(async () => ({ initialized: true, viewerPublic: true }));
		const enterPublicViewer = vi.fn(async () => ({ success: true }));
		const revision = '1.0' as CredentialRevision;
		useProvider({
			login: async () => ({ success: true }),
			logout: async () => {},
			resolve: answer,
			credentialRevision: () => revision,
			onCredentialChanged: () => () => {},
			status,
			enterPublicViewer
		} as AuthProvider);
		return { status, enterPublicViewer, revision };
	}

	it('エラー画面にし、閲覧者への切り替えもログイン画面も選ばない', async () => {
		const { status, enterPublicViewer } = tauriLike(async () => {
			throw new Error('storage: database is locked');
		});

		expect(await runGuard()).toMatchObject({ kind: 'error', status: 503 });
		expect(status).not.toHaveBeenCalled();
		expect(enterPublicViewer).not.toHaveBeenCalled();
	});

	it('セッション無し（none）はログイン画面へ', async () => {
		const revision = '1.0' as CredentialRevision;
		tauriLike(async () => ({ status: 'none', checked: revision, current: revision }));
		expect(await runGuard()).toEqual({ kind: 'redirect', location: '/login' });
	});
});

describe('isBantoAuthCheckResponse: 組み込みサーバーの判定', () => {
	it('200 / 401 はこのサーバー', async () => {
		expect(await isBantoAuthCheckResponse(jsonResponse(200, false))).toBe(true);
		expect(await isBantoAuthCheckResponse(jsonResponse(401, { kind: 'unauthorized' }))).toBe(true);
	});

	it('照合の 500（Banto のエラー本文）もこのサーバー - デモ用プロバイダーへ落ちない', async () => {
		expect(
			await isBantoAuthCheckResponse(
				jsonResponse(500, { kind: 'storage', message: 'database is locked' })
			)
		).toBe(true);
	});

	it('Banto の本文を持たない応答（vite dev の HTML 404 など）はサーバー無し', async () => {
		expect(
			await isBantoAuthCheckResponse(
				new Response('<!doctype html><title>404</title>', { status: 404 })
			)
		).toBe(false);
		expect(await isBantoAuthCheckResponse(jsonResponse(500, { error: 'x' }))).toBe(false);
	});
});
