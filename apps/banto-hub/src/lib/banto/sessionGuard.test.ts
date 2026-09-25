/**
 * `sessionGuard.ts` のユニットテスト（banto v1.7.0 #204）。
 *
 * 守りたいこと: サーバーがセッションを**照合できなかった**とき（照合の
 * `500`・到達不能）、ルートガードは
 * - `'unverified'`（エラー画面と再試行）にし、`'login'` にしない
 * - 保存しているトークン（通常の `sessionStorage` も Remember me の
 *   `localStorage` も）を消さない
 * - `status()` / `enterPublicViewer()`（閲覧者への切り替え）を呼ばない
 *
 * **本物の** `@banto/admin-core` の HTTP 認証プロバイダーと
 * `resolveProtectedSession` を使う（v1.7.0 で `check()` が 500 を reject する
 * ようになった挙動そのものを固定したいため）。ただし admin-core のパッケージ
 * 入口は Svelte 5 rune の `.svelte.ts` を推移的に読み込み、このリポジトリの
 * 最小 vitest 構成では読めない（`commissioning.test.ts` の doc comment
 * 参照）ので、依存の無い 2 ファイルをパッケージ内のパスから直接読む。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@banto/admin-core', async () => {
	const gate = await import('../../../node_modules/@banto/admin-core/src/sessionGate');
	return { resolveProtectedSession: gate.resolveProtectedSession };
});

import { createHttpAuthProvider } from '../../../node_modules/@banto/admin-core/src/providers/http';
import { decideProtectedRoute } from './sessionGuard';

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

/** A provider whose `fetch` answers `/api/auth/check` with `check`, and records every path asked for. */
function providerWith(check: () => Promise<Response>) {
	const paths: string[] = [];
	const fetchFn = vi.fn(async (url: string | URL | Request) => {
		const path = String(url);
		paths.push(path);
		if (path.endsWith('/api/auth/check')) return check();
		return jsonResponse(200, { initialized: true, viewerPublic: true });
	}) as unknown as typeof fetch;
	return { auth: createHttpAuthProvider({ fetchFn }), paths };
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

describe('decideProtectedRoute: 照合できないときはトークンを残してエラー画面', () => {
	for (const [label, check] of unverifiedCases) {
		it(`${label}: 通常のセッション`, async () => {
			session.setItem(TOKEN_KEY, 'normal-token');
			const { auth, paths } = providerWith(check);

			expect(await decideProtectedRoute(auth)).toBe('unverified');
			expect(session.getItem(TOKEN_KEY)).toBe('normal-token');
			// 閲覧者への切り替え（status → enterPublicViewer）もしない
			expect(paths.every((path) => path.endsWith('/api/auth/check'))).toBe(true);
		});

		it(`${label}: Remember me のセッション`, async () => {
			local.setItem(TOKEN_KEY, 'remembered-token');
			const { auth, paths } = providerWith(check);

			expect(await decideProtectedRoute(auth)).toBe('unverified');
			expect(local.getItem(TOKEN_KEY)).toBe('remembered-token');
			expect(paths.every((path) => path.endsWith('/api/auth/check'))).toBe(true);
		});
	}
});

describe('decideProtectedRoute: 確認できたときだけ判断する', () => {
	it('401（失効）はログイン画面へ。トークンは消える', async () => {
		local.setItem(TOKEN_KEY, 'revoked-token');
		const { auth } = providerWith(async () => jsonResponse(401, { kind: 'unauthorized' }));

		expect(await decideProtectedRoute(auth)).toBe('login');
		expect(local.getItem(TOKEN_KEY)).toBeNull();
	});

	it('トークンが無ければログイン画面へ', async () => {
		const { auth } = providerWith(async () => jsonResponse(200, true));
		expect(await decideProtectedRoute(auth)).toBe('login');
	});

	it('200 true はそのまま進む', async () => {
		session.setItem(TOKEN_KEY, 'live-token');
		const { auth } = providerWith(async () => jsonResponse(200, true));

		expect(await decideProtectedRoute(auth)).toBe('session');
		expect(session.getItem(TOKEN_KEY)).toBe('live-token');
	});
});
