/**
 * (app) ルートガード（`routes/(app)/+layout.ts`）と `sessionGuard.ts` の
 * ユニットテスト（banto v1.7.0 #204、v2.0.0 #260 で書き直し）。
 *
 * 守りたいこと: サーバーがセッションを**照合できなかった**とき（照合の
 * `500`・到達不能）、ルートガードは
 * - 503（エラー画面と再試行）にし、/login へ送らない
 * - 保存しているトークン（通常の `sessionStorage` も Remember me の
 *   `localStorage` も）を消さない
 * - `status()` / `enterGrant()`（grant の発行、`grantFallback`）を呼ばない
 *   （照合できないうちは閲覧公開へも切り替えない）
 *
 * I2b（2026-10-04 オーナー決定: ChronoGazer でも閲覧公開を使う）: 確定した
 * `none` は admin-template と同じく `grantFallback(..., { kind: 'publicViewer' })`
 * を通る。閲覧公開が ON（`status()` の `grants.publicViewer`）なら閲覧者の
 * セッションに入り、許可リスト（`NavItem.publicViewer`）の外の画面は
 * 先頭（/monitor）へ移す。OFF なら /login。
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
// `afterConfirm`: a test hook run inside the guard right after its confirmation
// (the guard calls `settings.syncFromProvider()` synchronously there), to move
// the session on before `load()` returns.
const hooks = vi.hoisted(() => ({ afterConfirm: null as null | (() => void) }));
vi.mock('$lib/settings.svelte', () => ({
	settings: {
		syncFromProvider: async () => {
			hooks.afterConfirm?.();
		}
	}
}));

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
	hooks.afterConfirm = null;
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

const GRANT_TOKEN = 'public-viewer-grant-token';

/**
 * A real HTTP provider whose `fetch` answers `/api/auth/identity` with
 * `identity`, recording every path asked for. `viewerPublic` is what
 * `/api/auth/status` reports as `grants.publicViewer`; while it is `true`,
 * `POST /api/auth/grant/publicViewer` issues `GRANT_TOKEN` (else `403`), and
 * `/api/auth/identity` with that token answers the fixed public identity.
 */
function httpProviderWith(identity: () => Promise<Response>, options = { viewerPublic: false }) {
	const paths: string[] = [];
	const fetchFn = vi.fn(async (url: string | URL | Request, init?: RequestInit) => {
		const path = String(url);
		paths.push(path);
		const headers = new Headers(init?.headers);
		if (path.endsWith('/api/auth/identity')) {
			if (headers.get('Authorization') === `Bearer ${GRANT_TOKEN}`) {
				return jsonResponse(200, {
					id: 'public',
					name: 'public',
					role: 'viewer',
					kind: 'publicViewer'
				});
			}
			return identity();
		}
		if (path.endsWith('/api/auth/grant/publicViewer')) {
			return options.viewerPublic
				? jsonResponse(200, { success: true, token: GRANT_TOKEN })
				: jsonResponse(403, { kind: 'forbidden', message: 'forbidden' });
		}
		return jsonResponse(200, {
			initialized: true,
			grants: { publicViewer: options.viewerPublic }
		});
	}) as unknown as typeof fetch;
	useProvider(createHttpAuthProvider({ fetchFn }));
	return { paths };
}

/** Whether a grant was requested (`POST /api/auth/grant/...`). */
function askedForGrant(paths: string[]): boolean {
	return paths.some((path) => path.includes('/api/auth/grant/'));
}

/** Run the guard; return what it threw (or its data). */
async function runGuard(
	pathname = '/monitor'
): Promise<
	| { kind: 'data'; sessionGeneration: number }
	| { kind: 'redirect'; location: string }
	| { kind: 'error'; status: number; message: string }
> {
	try {
		const data = await load({
			url: new URL(`http://127.0.0.1:8721${pathname}`)
		} as Parameters<typeof load>[0]);
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
			// grant の発行（status → enterGrant）もしない
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
	it('401（失効）はログイン画面へ。トークンは消える。閲覧公開 OFF なら閲覧者への切り替えはしない', async () => {
		local.setItem(TOKEN_KEY, 'revoked-token');
		const { paths } = httpProviderWith(async () => jsonResponse(401, { kind: 'unauthorized' }));

		expect(await runGuard()).toEqual({ kind: 'redirect', location: '/login' });
		expect(local.getItem(TOKEN_KEY)).toBeNull();
		expect(askedForGrant(paths)).toBe(false);
	});

	it('トークンが無く閲覧公開 OFF なら、identity を問い合わせずにログイン画面へ', async () => {
		const { paths } = httpProviderWith(async () => jsonResponse(200, null));
		expect(await runGuard()).toEqual({ kind: 'redirect', location: '/login' });
		expect(paths.some((path) => path.endsWith('/api/auth/identity'))).toBe(false);
		expect(askedForGrant(paths)).toBe(false);
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

	it('返す世代は「この load が確定した世代」で、load の後に動いた今の世代ではない（I-16）', async () => {
		let revision = 1;
		const listeners = new Set<() => void>();
		const rev = () => `${revision}.0` as CredentialRevision;
		useProvider({
			login: async () => ({ success: true }),
			logout: async () => {},
			resolve: async () => ({
				status: 'active',
				checked: rev(),
				current: rev(),
				identity: { id: 'alice', name: 'Alice', role: 'admin' }
			}),
			credentialRevision: rev,
			onCredentialChanged(listener) {
				listeners.add(listener);
				return () => listeners.delete(listener);
			}
		});
		const controller = getSessionController();
		let confirmedGeneration = -1;
		// Another tab's login lands right after this load confirmed Alice: the
		// controller holds (generation moves) before the load returns.
		hooks.afterConfirm = () => {
			confirmedGeneration = controller.snapshot.generation;
			revision += 1;
			for (const listener of [...listeners]) listener();
		};

		const result = await runGuard();
		expect(controller.snapshot.generation).not.toBe(confirmedGeneration);
		expect(result).toEqual({ kind: 'data', sessionGeneration: confirmedGeneration });
	});
});

describe('(app) ガード: 閲覧公開（publicViewer grant、I2b）', () => {
	it('ON なら未ログインは閲覧者のセッションに入り、許可された画面はそのまま開く', async () => {
		const { paths } = httpProviderWith(async () => jsonResponse(200, null), {
			viewerPublic: true
		});

		const result = await runGuard('/historical');
		const snapshot = getSessionController().snapshot;
		expect(snapshot).toMatchObject({ status: 'active', kind: 'publicViewer' });
		expect(result).toEqual({ kind: 'data', sessionGeneration: snapshot.generation });
		expect(askedForGrant(paths)).toBe(true);
		// 閲覧公開のトークンは Remember me しない（sessionStorage だけ）
		expect(session.getItem(TOKEN_KEY)).toBe(GRANT_TOKEN);
		expect(local.getItem(TOKEN_KEY)).toBeNull();
	});

	it('許可リストの外（タグ設定・設定・ユーザー管理）は先頭の /monitor へ移す', async () => {
		for (const pathname of ['/tags', '/settings/appearance', '/users', '/audit-log']) {
			session.clear();
			httpProviderWith(async () => jsonResponse(200, null), { viewerPublic: true });
			expect(await runGuard(pathname)).toEqual({ kind: 'redirect', location: '/monitor' });
		}
	});

	it('ログイン済みのアカウントは閲覧公開 ON でも grant を求めず、どの画面も開く', async () => {
		session.setItem(TOKEN_KEY, 'live-token');
		const { paths } = httpProviderWith(
			async () => jsonResponse(200, { id: 'alice', name: 'Alice', role: 'viewer' }),
			{ viewerPublic: true }
		);

		expect(await runGuard('/tags')).toMatchObject({ kind: 'data' });
		expect(askedForGrant(paths)).toBe(false);
	});
});

describe('(app) ガード: Tauri の auth_resolve が DB エラーで reject したとき', () => {
	/** A standard provider (as the Tauri one) whose `resolve()` is `answer`. */
	function tauriLike(answer: AuthProvider['resolve']) {
		const status = vi.fn(async () => ({ initialized: true, grants: {} }));
		const enterGrant = vi.fn(async () => ({ success: true }));
		const revision = '1.0' as CredentialRevision;
		useProvider({
			login: async () => ({ success: true }),
			logout: async () => {},
			resolve: answer,
			credentialRevision: () => revision,
			onCredentialChanged: () => () => {},
			status,
			enterGrant
		} as AuthProvider);
		return { status, enterGrant, revision };
	}

	it('エラー画面にし、grant の発行もログイン画面も選ばない', async () => {
		const { status, enterGrant } = tauriLike(async () => {
			throw new Error('storage: database is locked');
		});

		expect(await runGuard()).toMatchObject({ kind: 'error', status: 503 });
		expect(status).not.toHaveBeenCalled();
		expect(enterGrant).not.toHaveBeenCalled();
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
