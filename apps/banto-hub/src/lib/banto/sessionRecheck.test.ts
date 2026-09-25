/**
 * `sessionRecheck.ts` と、それが走らせ直すルートガード（`(app)/+layout.ts`）
 * のユニットテスト（#441）。
 *
 * 守りたいこと: ストリームが `1008` + `session_revoked` /
 * `commissioning_ended` で閉じたときの確認は、画面を開いたときと同じルート
 * ガードを通る。
 * - 失効を確認できた（`401`）→ `/login` へ。トークンは消える
 * - 照合できなかった（`500`・到達不能）→ 再試行付きのエラー画面（`503`）。
 *   トークンは消さない
 * - まだ有効 → そのまま（画面に残る）
 * - ロックダウンされた（`commissioning_ended`）→ ログインが要る側へ進む
 * - 複数のストリームが同時に閉じても、確認は 1 回
 *
 * #445: 再接続が続けて失敗したときの確認（`probeSessionAfterReconnectFailures`）
 * は画面を動かさず（`invalidateAll()` を呼ばない）、ルートガードと同じ部品で
 * `session` / `login` / `unverified` を返す。照合できない・状態を読めない・
 * 返ってこない（時間切れ）は `unverified`（試運転モード中のネットワーク断で
 * ログイン画面へ送らない）。
 *
 * `invalidateAll()`（`$app/navigation`）は SvelteKit の実行時が無いと動かない
 * ので、呼ばれた回数だけ数える。ルートガードは `load()` を直接呼び、
 * `@banto/admin-core` は `sessionGuard.test.ts` と同じく**本物の** HTTP 認証
 * プロバイダーと `resolveProtectedSession` を使う。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { isHttpError, isRedirect } from '@sveltejs/kit';

const nav = vi.hoisted(() => ({
	invalidateAll: vi.fn<() => Promise<void>>(async () => {})
}));
const state = vi.hoisted(() => ({
	commissioning: { lockedDown: true } as { lockedDown: boolean } | null,
	auth: null as unknown,
	sessionLoad: vi.fn(async () => {}),
	enterCommissioningMode: vi.fn(),
	commissioningMode: false
}));

vi.mock('$app/navigation', () => ({ invalidateAll: nav.invalidateAll }));
vi.mock('@banto/admin-core', async () => {
	const gate = await import('../../../node_modules/@banto/admin-core/src/sessionGate');
	return {
		resolveProtectedSession: gate.resolveProtectedSession,
		getAuthProvider: () => state.auth
	};
});
vi.mock('$lib/banto/setup', () => ({ bantoReady: Promise.resolve() }));
vi.mock('$lib/banto/sessionGuard', () => import('./sessionGuard'));
vi.mock('$lib/banto/commissioning', () => ({
	fetchCommissioningStatusOrNull: async () => state.commissioning,
	shouldBypassLoginForCommissioning: (status: { lockedDown: boolean } | null) =>
		status !== null && !status.lockedDown
}));
vi.mock('$lib/session.svelte', () => ({
	sessionStore: {
		load: state.sessionLoad,
		enterCommissioningMode: state.enterCommissioningMode,
		get commissioningMode() {
			return state.commissioningMode;
		}
	}
}));
vi.mock('$lib/settings.svelte', () => ({ settings: { syncFromProvider: async () => {} } }));

import { createHttpAuthProvider } from '../../../node_modules/@banto/admin-core/src/providers/http';
import { load } from '../../routes/(app)/+layout';
import {
	probeSessionAfterReconnectFailures,
	recheckSessionAfterStreamClose,
	SESSION_PROBE_TIMEOUT_MS
} from './sessionRecheck';
import { SESSION_CHECK_FAILED_MESSAGE } from './sessionGuard';

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

let session: MemoryStorage;

function jsonResponse(status: number, body: unknown): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'content-type': 'application/json' }
	});
}

function useCheck(check: () => Promise<Response>): void {
	const fetchFn = vi.fn(async (url: string | URL | Request) => {
		if (String(url).endsWith('/api/auth/check')) return check();
		return jsonResponse(200, { initialized: true, viewerPublic: false });
	}) as unknown as typeof fetch;
	state.auth = createHttpAuthProvider({ fetchFn });
}

/** ルートガードを走らせ、投げたもの（redirect / error）か `'passed'` を返す。 */
async function runGuard(): Promise<unknown> {
	try {
		await load();
		return 'passed';
	} catch (thrown) {
		return thrown;
	}
}

beforeEach(() => {
	session = new MemoryStorage();
	vi.stubGlobal('sessionStorage', session);
	vi.stubGlobal('localStorage', new MemoryStorage());
	state.commissioning = { lockedDown: true };
	state.commissioningMode = false;
	state.sessionLoad.mockClear();
	state.enterCommissioningMode.mockClear();
	nav.invalidateAll.mockReset();
	nav.invalidateAll.mockImplementation(async () => {});
});

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('recheckSessionAfterStreamClose', () => {
	it('ルートガードを走らせ直す（invalidateAll）', async () => {
		await recheckSessionAfterStreamClose();
		expect(nav.invalidateAll).toHaveBeenCalledTimes(1);
	});

	it('複数のストリームが同時に閉じても、確認は 1 回', async () => {
		let release: () => void = () => {};
		nav.invalidateAll.mockImplementation(
			() =>
				new Promise<void>((resolve) => {
					release = resolve;
				})
		);
		const pending = [
			recheckSessionAfterStreamClose(),
			recheckSessionAfterStreamClose(),
			recheckSessionAfterStreamClose()
		];
		release();
		await Promise.all(pending);
		expect(nav.invalidateAll).toHaveBeenCalledTimes(1);

		// 終わった後にまた閉じられたら、改めて確認する。
		nav.invalidateAll.mockImplementation(async () => {});
		await recheckSessionAfterStreamClose();
		expect(nav.invalidateAll).toHaveBeenCalledTimes(2);
	});
});

describe('走らせ直したルートガード（session_revoked の後）', () => {
	it('失効を確認できた（401）→ /login へ。トークンは消える', async () => {
		session.setItem(TOKEN_KEY, 'revoked-token');
		useCheck(async () => jsonResponse(401, { kind: 'unauthorized' }));

		const thrown = await runGuard();
		expect(isRedirect(thrown)).toBe(true);
		if (isRedirect(thrown)) expect(thrown.location).toBe('/login');
		expect(session.getItem(TOKEN_KEY)).toBeNull();
		expect(state.sessionLoad).not.toHaveBeenCalled();
	});

	it.each([
		[
			'照合の 500',
			async () => jsonResponse(500, { kind: 'storage', message: 'database is locked' })
		],
		[
			'到達不能',
			async (): Promise<Response> => {
				throw new TypeError('Failed to fetch');
			}
		]
	])('照合できなかった（%s）→ 再試行付きのエラー画面。トークンは残る', async (_label, check) => {
		session.setItem(TOKEN_KEY, 'live-token');
		useCheck(check);

		const thrown = await runGuard();
		expect(isHttpError(thrown)).toBe(true);
		if (isHttpError(thrown)) {
			expect(thrown.status).toBe(503);
			expect(thrown.body.message).toBe(SESSION_CHECK_FAILED_MESSAGE);
		}
		expect(session.getItem(TOKEN_KEY)).toBe('live-token');
	});

	it('まだ有効（200 true）→ そのまま進む（画面に残り、呼び出し側が購読を再開する）', async () => {
		session.setItem(TOKEN_KEY, 'live-token');
		useCheck(async () => jsonResponse(200, true));

		expect(await runGuard()).toBe('passed');
		expect(state.sessionLoad).toHaveBeenCalledTimes(1);
	});
});

describe('走らせ直したルートガード（commissioning_ended の後）', () => {
	it('ロックダウン済みでトークンが無ければ /login へ（試運転モードの迂回はしない）', async () => {
		state.commissioning = { lockedDown: true };
		useCheck(async () => jsonResponse(200, true));

		const thrown = await runGuard();
		expect(isRedirect(thrown)).toBe(true);
		if (isRedirect(thrown)) expect(thrown.location).toBe('/login');
		expect(state.enterCommissioningMode).not.toHaveBeenCalled();
	});

	it('状態を読めなかったときも安全側（ログインが要る側）へ倒す', async () => {
		state.commissioning = null;
		useCheck(async () => jsonResponse(200, true));

		const thrown = await runGuard();
		expect(isRedirect(thrown)).toBe(true);
		expect(state.enterCommissioningMode).not.toHaveBeenCalled();
	});
});

describe('probeSessionAfterReconnectFailures（#445: 再接続が続けて失敗したとき）', () => {
	it.each([
		['200 true', async () => jsonResponse(200, true), 'session'],
		['200 false（失効したセッション）', async () => jsonResponse(200, false), 'login'],
		['401', async () => jsonResponse(401, { kind: 'unauthorized' }), 'login'],
		[
			'照合の 500',
			async () => jsonResponse(500, { kind: 'storage', message: 'database is locked' }),
			'unverified'
		],
		[
			'到達不能',
			async (): Promise<Response> => {
				throw new TypeError('Failed to fetch');
			},
			'unverified'
		]
	] as const)(
		'ロックダウン済み・照合 %s → %s。画面は動かさない',
		async (_label, check, expected) => {
			session.setItem(TOKEN_KEY, 'tok');
			useCheck(check);

			expect(await probeSessionAfterReconnectFailures()).toBe(expected);
			expect(nav.invalidateAll).not.toHaveBeenCalled();
		}
	);

	it('照合できないときはトークンを消さない', async () => {
		session.setItem(TOKEN_KEY, 'live-token');
		useCheck(async () => jsonResponse(500, { kind: 'storage', message: 'x' }));

		expect(await probeSessionAfterReconnectFailures()).toBe('unverified');
		expect(session.getItem(TOKEN_KEY)).toBe('live-token');
	});

	it('トークンが無ければ login', async () => {
		useCheck(async () => jsonResponse(200, true));
		expect(await probeSessionAfterReconnectFailures()).toBe('login');
	});

	it('試運転モードの状態が読めなかった（ネットワーク断など）→ unverified（ガードと違い、ログインへ倒さない）', async () => {
		state.commissioning = null;
		state.commissioningMode = true;
		useCheck(async () => jsonResponse(200, true));

		expect(await probeSessionAfterReconnectFailures()).toBe('unverified');
	});

	it('試運転モードのまま → session（照合しない）', async () => {
		state.commissioning = { lockedDown: false };
		state.commissioningMode = true;
		const check = vi.fn(async () => jsonResponse(401, { kind: 'unauthorized' }));
		useCheck(check);

		expect(await probeSessionAfterReconnectFailures()).toBe('session');
		expect(check).not.toHaveBeenCalled();
	});

	it('試運転モードのつもりで繋いでいたが、ロックダウンされていた → login（ガードを走らせ直す）', async () => {
		state.commissioning = { lockedDown: true };
		state.commissioningMode = true;
		session.setItem(TOKEN_KEY, 'tok');
		useCheck(async () => jsonResponse(200, true));

		expect(await probeSessionAfterReconnectFailures()).toBe('login');
	});

	it('返ってこなければ、時間切れで unverified（次の確認を起こせるように）', async () => {
		vi.useFakeTimers();
		try {
			session.setItem(TOKEN_KEY, 'tok');
			useCheck(() => new Promise<Response>(() => {}));

			const result = probeSessionAfterReconnectFailures();
			await vi.advanceTimersByTimeAsync(SESSION_PROBE_TIMEOUT_MS);
			expect(await result).toBe('unverified');

			// 時間切れの後は新しく確認できる。
			useCheck(async () => jsonResponse(200, false));
			expect(await probeSessionAfterReconnectFailures()).toBe('login');
		} finally {
			vi.useRealTimers();
		}
	});

	it('同時に何度呼ばれても照合は 1 回（single-flight）', async () => {
		session.setItem(TOKEN_KEY, 'tok');
		let release: () => void = () => {};
		const check = vi.fn(
			() =>
				new Promise<Response>((resolve) => {
					release = () => resolve(jsonResponse(200, true));
				})
		);
		useCheck(check);

		const pending = [
			probeSessionAfterReconnectFailures(),
			probeSessionAfterReconnectFailures(),
			probeSessionAfterReconnectFailures()
		];
		await vi.waitFor(() => expect(check).toHaveBeenCalledTimes(1));
		release();
		expect(await Promise.all(pending)).toEqual(['session', 'session', 'session']);
		expect(check).toHaveBeenCalledTimes(1);
	});
});
