/**
 * #445 の確認（`probeSessionAfterReconnectFailures`）の時間切れで、確認の中で
 * 待っている**すべての** HTTP 要求が止まること（PR #447 の再レビュー）。
 *
 * `sessionRecheck.test.ts` は試運転の状態の取得（`$lib/banto/commissioning`）を
 * 丸ごと即時応答に差し替えているので、その要求が止まるかは見えない。ここでは
 * `commissioning.ts` の本物の HTTP ヘルパーを通し、偽の `fetch`（本物と同じく、
 * 中断されたら `AbortError` で reject する）で要求ごとに `signal` を見る。
 *
 * 守りたいこと:
 * - `/api/commissioning/status` が応答しないまま 10 秒経つと、その要求の
 *   `signal.aborted === true`（止まっている）。確認を繰り返しても、止まって
 *   いない要求が残らない（積み重ならない）。
 * - 対照: 状態は返り、`/api/auth/check` が応答しないときも同じく止まる。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const auth = vi.hoisted(() => ({ token: 'tok' as string | null }));

vi.mock('$app/navigation', () => ({ invalidateAll: async () => {} }));
vi.mock('@banto/admin-core', () => ({
	getAuthProvider: () => ({ getToken: () => auth.token }),
	ProviderError: class ProviderError extends Error {}
}));
vi.mock('./setup', () => ({ CSRF_HEADER: { 'X-Banto-Client': 'banto' } }));
vi.mock('$lib/banto/setup', () => ({ CSRF_HEADER: { 'X-Banto-Client': 'banto' } }));
// この最小 vitest 構成には `$lib` の別名が無いので、本物のモジュールへ向ける
// （`sessionRecheck.test.ts` の `sessionGuard` と同じ書き方。差し替えではない）。
vi.mock('$lib/banto/commissioning', () => import('./commissioning'));
vi.mock('$lib/session.svelte', () => ({ sessionStore: { commissioningMode: false } }));

import { probeSessionAfterReconnectFailures, SESSION_PROBE_TIMEOUT_MS } from './sessionRecheck';

interface SentRequest {
	path: string;
	signal: AbortSignal | null | undefined;
	settled: boolean;
}

let sent: SentRequest[] = [];

/** 応答しない要求。本物の `fetch` と同じく、中断されたら reject する。 */
function hangingRequest(request: SentRequest): Promise<Response> {
	return new Promise<Response>((_resolve, reject) => {
		request.signal?.addEventListener('abort', () => {
			request.settled = true;
			reject(new DOMException('The operation was aborted.', 'AbortError'));
		});
	});
}

function useFetch(answer: (request: SentRequest) => Promise<Response>): void {
	vi.stubGlobal(
		'fetch',
		vi.fn(async (url: string | URL | Request, init?: RequestInit) => {
			const request: SentRequest = { path: String(url), signal: init?.signal, settled: false };
			sent.push(request);
			return answer(request);
		})
	);
}

const pending = () => sent.filter((r) => !r.settled);

beforeEach(() => {
	vi.useFakeTimers();
	sent = [];
	auth.token = 'tok';
});

afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

describe('確認の時間切れで、確認の中の要求をすべて止める（PR #447 の再レビュー）', () => {
	it('/api/commissioning/status が応答しないまま 10 秒経つと、その要求は止まり、繰り返しても残らない', async () => {
		useFetch((request) => hangingRequest(request));

		for (let round = 1; round <= 3; round++) {
			const result = probeSessionAfterReconnectFailures();
			await vi.advanceTimersByTimeAsync(SESSION_PROBE_TIMEOUT_MS);
			expect(await result).toBe('unverified');

			expect(sent).toHaveLength(round);
			const request = sent[round - 1];
			expect(request.path).toBe('/api/commissioning/status');
			expect(request.signal?.aborted).toBe(true);
			expect(pending()).toHaveLength(0);
		}
		// 試運転の状態が分からないので、照合（/api/auth/check）へは進んでいない。
		expect(sent.every((r) => r.path === '/api/commissioning/status')).toBe(true);
	});

	it('対照: 状態は返り、/api/auth/check が応答しないときも、同じ期限で止まる', async () => {
		useFetch((request) =>
			request.path === '/api/commissioning/status'
				? Promise.resolve(
						new Response(JSON.stringify({ lockedDown: true }), {
							status: 200,
							headers: { 'content-type': 'application/json' }
						})
					)
				: hangingRequest(request)
		);

		const result = probeSessionAfterReconnectFailures();
		await vi.advanceTimersByTimeAsync(SESSION_PROBE_TIMEOUT_MS);
		expect(await result).toBe('unverified');

		expect(sent.map((r) => r.path)).toEqual(['/api/commissioning/status', '/api/auth/check']);
		// 2 つの要求は同じ期限（同じ signal）を共有する。
		expect(sent[0].signal).toBe(sent[1].signal);
		expect(sent[1].signal?.aborted).toBe(true);
		expect(sent[1].settled).toBe(true);
	});
});
