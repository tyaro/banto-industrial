/**
 * #445 の確認（`probeSessionAfterReconnectFailures`）の時間切れで、確認の中で
 * 待っている**すべての** HTTP 要求が止まること（PR #447 の再レビュー、実装
 * チェックリスト §5「時間切れで見捨てた非同期処理の副作用」。banto v2.0.0 #260
 * で書き直し）。
 *
 * 試運転の状態の取得を丸ごと差し替えると、その要求が止まるかは見えない。
 * ここでは `commissioning.ts` の本物の HTTP ヘルパーと、本物の
 * `@banto/admin-core`（HTTP 認証プロバイダーと既定の controller）を通し、偽の
 * `fetch`（本物と同じく、中断されたら `AbortError` で reject する）で要求ごとに
 * `signal` を見る。
 *
 * 守りたいこと:
 * - 試運転中（policy runner の `recheck`）: `/api/commissioning/status` が応答
 *   しないまま 10 秒経つと、その要求の `signal.aborted === true`（runner の
 *   signal）。確認を繰り返しても、止まっていない要求が残らない（積み重ならない）。
 * - 試運転中で、状態は返り（ロックダウン済み）、通常の確認
 *   （`/api/auth/identity`）が応答しないとき: 全体で同じ 10 秒の期限で
 *   `unverified` になり、確認の要求は controller が止める（runner の signal は
 *   共有の probe に渡さない、I-22）。
 * - ロックダウン済み（試運転でない）: 確認は identity の 1 往復だけ。応答しない
 *   要求は 10 秒で止まり、繰り返しても積み重ならない。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { getSessionController } from '@banto/admin-core';

vi.mock('$app/navigation', () => ({ invalidateAll: async () => {} }));
vi.mock('./setup', () => ({ CSRF_HEADER: { 'X-Banto-Client': 'banto' } }));
vi.mock('$lib/banto/commissioningPolicy', () => import('./commissioningPolicy'));

import { COMMISSIONING_IDENTITY } from './commissioning';
import { COMMISSIONING_KIND } from './commissioningPolicy';
import { probeSessionAfterReconnectFailures } from './sessionRecheck';
import { installHub, jsonResponse, TOKEN_KEY, type SentRequest } from './testing/hubHttp';

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

function enterCommissioning(): void {
	const controller = getSessionController();
	expect(controller.adopt(COMMISSIONING_IDENTITY, COMMISSIONING_KIND, controller.ticket())).toBe(
		true
	);
}

describe('確認の時間切れで、確認の中の要求をすべて止める', () => {
	it('試運転中: /api/commissioning/status が応答しないまま 10 秒経つと、その要求は止まり、繰り返しても残らない', async () => {
		enterCommissioning();
		hub.routes.status = async (request) => hanging(request);

		for (let round = 1; round <= 3; round++) {
			const result = probeSessionAfterReconnectFailures();
			await vi.advanceTimersByTimeAsync(DEADLINE_MS);
			expect(await result).toBe('unverified');

			const statusRequests = hub.sent.filter((r) => r.path === '/api/commissioning/status');
			expect(statusRequests).toHaveLength(round);
			expect(statusRequests[round - 1].signal?.aborted).toBe(true);
			expect(pending('/api/commissioning/status')).toHaveLength(0);
		}
		// 試運転の状態が分からないので、試運転は終わらせず、通常の確認へも進んでいない。
		expect(getSessionController().snapshot.kind).toBe(COMMISSIONING_KIND);
		expect(hub.paths().every((p) => p === '/api/commissioning/status')).toBe(true);
	});

	it('試運転中・状態は返り（ロックダウン済み）、/api/auth/identity が応答しない: 全体で 10 秒、確認の要求も止まる', async () => {
		enterCommissioning();
		hub.session.setItem(TOKEN_KEY, 'tok');
		hub.routes.status = () =>
			new Promise<Response>((resolve) => {
				setTimeout(() => resolve(jsonResponse(200, { lockedDown: true })), 4_000);
			});
		hub.routes.identity = async (request) => hanging(request);

		let done = false;
		const result = probeSessionAfterReconnectFailures().then((r) => {
			done = true;
			return r;
		});
		await vi.advanceTimersByTimeAsync(DEADLINE_MS - 1);
		expect(done).toBe(false);
		await vi.advanceTimersByTimeAsync(1);
		expect(await result).toBe('unverified');

		const identity = hub.sent.find((r) => r.path === '/api/auth/identity');
		expect(identity).toBeDefined();
		// runner の signal は共有の probe に渡さない（I-22）。probe は controller の期限で止まる。
		expect(identity?.signal).not.toBe(hub.sent[0].signal);
		await vi.advanceTimersByTimeAsync(DEADLINE_MS);
		expect(identity?.signal?.aborted).toBe(true);
	});

	it('ロックダウン済み: 確認は identity の 1 往復。応答しない要求は 10 秒で止まり、積み重ならない', async () => {
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
