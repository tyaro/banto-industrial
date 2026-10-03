/**
 * #445 の確認（`probeSessionAfterReconnectFailures`）の時間切れで、確認の中で
 * 待っている HTTP 要求が止まること（PR #447 の再レビュー、実装チェックリスト §5
 * 「時間切れで見捨てた非同期処理の副作用」）。
 *
 * banto v3.0.0（ADR-0017）で試運転の policy runner（`commissioningPolicy.ts`、
 * 試運転の状態の取得を自前の signal で止めていた）は無くなり、確認は常に
 * controller の `resolveSettled(controller, { cause: 'signal' })`（`GET
 * /api/auth/identity` の 1 往復、期限 10 秒は controller が持つ）になった。
 * 試運転の grant のトークンも通常のトークンと同じ経路なので、ここで守るのは
 * その 1 往復が止まること。本物の `@banto/admin-core`（HTTP 認証プロバイダーと
 * 既定の controller）を通し、偽の `fetch`（本物と同じく、中断されたら
 * `AbortError` で reject する）で要求ごとに `signal` を見る。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$app/navigation', () => ({ invalidateAll: async () => {} }));
vi.mock('./setup', () => ({ CSRF_HEADER: { 'X-Banto-Client': 'banto' } }));

import { probeSessionAfterReconnectFailures } from './sessionRecheck';
import { installHub, TOKEN_KEY, type SentRequest } from './testing/hubHttp';

const DEADLINE_MS = 10_000;

let hub: ReturnType<typeof installHub>;
const settled = new Set<SentRequest>();

/** 応答しない要求。本物の `fetch` と同じく、中断されたら reject する。 */
function hanging(request: SentRequest): Promise<Response> {
	return new Promise<Response>((_resolve, reject) => {
		request.signal?.addEventListener('abort', () => {
			settled.add(request);
			reject(new DOMException('The operation was aborted.', 'AbortError'));
		});
	});
}

const pending = (path: string) =>
	hub.sent.filter((r) => r.path === path && !settled.has(r) && !r.signal?.aborted);

beforeEach(() => {
	vi.useFakeTimers();
	settled.clear();
	hub = installHub();
});

afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

describe('確認の時間切れで、確認の中の要求をすべて止める', () => {
	it('確認は identity の 1 往復。応答しない要求は 10 秒で止まり、積み重ならない', async () => {
		hub.session.setItem(TOKEN_KEY, 'tok');
		hub.routes.identity = async (request) => hanging(request);

		for (let round = 1; round <= 3; round++) {
			const result = probeSessionAfterReconnectFailures();
			await vi.advanceTimersByTimeAsync(DEADLINE_MS);
			expect(await result).toBe('unverified');
			// 背景の確認（1 秒から倍々）も同じ規則で止まるので、生きている要求は高々 1 本。
			expect(pending('/api/auth/identity').length).toBeLessThanOrEqual(1);
		}
		expect(hub.paths()).not.toContain('/api/commissioning/status');
		expect(hub.paths()).not.toContain('/api/auth/check');
		expect(hub.session.getItem(TOKEN_KEY)).toBe('tok');
	});
});
