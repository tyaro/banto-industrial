/**
 * `logFeedState.svelte.ts` のユニットテスト（#1253）。取得と保持の判断は `logFeed.test.ts`
 * が固定し、ここは配線（回す条件・一時停止・初回から読み直したときの控えの破棄・口の URL）。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';

vi.mock('@banto/admin-core', () => ({
	getAuthProvider: () => ({ getToken: () => 'tok' })
}));
vi.mock('./setup', () => ({ CSRF_HEADER: { 'X-Banto-Client': 'banto' } }));

import { createLogFeedState, fetchLogFeed } from './logFeedState.svelte';
import type { LogFeedEntry, LogFeedFetchOutcome } from './logFeed';

function conn(key: string, ts: number): LogFeedEntry {
	return {
		source: 'connection',
		key,
		ts_ms: ts,
		kind: 'plc_connected',
		connection_id: 1,
		connection_name: 'PLC1',
		detail: null,
		transport: null,
		reason: null
	};
}

async function flush(): Promise<void> {
	for (let i = 0; i < 10; i++) await Promise.resolve();
}

afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

describe('fetchLogFeed', () => {
	it('初回は cursor 無し、続きは cursor を付け、トークンと CSRF のヘッダーを送る', async () => {
		const fetchMock = vi.fn(async () => ({
			status: 200,
			json: async () => ({ entries: [], cursor: 'k', more: false })
		}));
		vi.stubGlobal('fetch', fetchMock);
		const signal = new AbortController().signal;
		expect((await fetchLogFeed(null, signal)).kind).toBe('ok');
		await fetchLogFeed('a+b/c', signal);
		const calls = fetchMock.mock.calls as unknown as [string, RequestInit][];
		expect(calls[0][0]).toBe('/api/log-feed');
		expect(calls[1][0]).toBe('/api/log-feed?cursor=a%2Bb%2Fc');
		expect(calls[0][1].headers).toEqual({ 'X-Banto-Client': 'banto', Authorization: 'Bearer tok' });
	});

	it('ネットワークの失敗は failed', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => {
				throw new TypeError('Failed to fetch');
			})
		);
		expect((await fetchLogFeed(null, new AbortController().signal)).kind).toBe('failed');
	});
});

describe('createLogFeedState', () => {
	function setup() {
		vi.useFakeTimers();
		const answers: LogFeedFetchOutcome[] = [];
		const cursors: (string | null)[] = [];
		const state = createLogFeedState({
			fetch: async (cursor) => {
				cursors.push(cursor);
				return (
					answers.shift() ?? { kind: 'ok', response: { entries: [], cursor: 'k', more: false } }
				);
			}
		});
		return { state, answers, cursors };
	}

	it('見えている間だけ読む', async () => {
		const { state, cursors } = setup();
		await vi.advanceTimersByTimeAsync(10_000);
		expect(cursors).toEqual([]);
		state.setVisible(true);
		await vi.advanceTimersByTimeAsync(0);
		expect(cursors).toEqual([null]);
		state.setVisible(false);
		await vi.advanceTimersByTimeAsync(20_000);
		expect(cursors).toEqual([null]);
		state.dispose();
	});

	it('一時停止は表示を止めて新しい行の件数を出し、再開で揃える', async () => {
		const { state, answers } = setup();
		answers.push({
			kind: 'ok',
			response: { entries: [conn('c:1', 100)], cursor: 'k1', more: false }
		});
		state.setVisible(true);
		await vi.advanceTimersByTimeAsync(0);
		await flush();
		expect(state.rows.map((r) => r.key)).toEqual(['c:1']);
		state.pause();
		answers.push({
			kind: 'ok',
			response: { entries: [conn('c:2', 200)], cursor: 'k2', more: false }
		});
		await vi.advanceTimersByTimeAsync(5000);
		await flush();
		expect(state.rows.map((r) => r.key)).toEqual(['c:1']);
		expect(state.pendingCount).toBe(1);
		state.resume();
		expect(state.rows.map((r) => r.key)).toEqual(['c:2', 'c:1']);
		expect(state.pendingCount).toBe(0);
		state.dispose();
	});

	it('一時停止中でも、初回から読み直したら止めた表示の行を捨てる', async () => {
		const { state, answers } = setup();
		answers.push({
			kind: 'ok',
			response: { entries: [conn('c:1', 100)], cursor: 'k1', more: false }
		});
		state.setVisible(true);
		await vi.advanceTimersByTimeAsync(0);
		await flush();
		state.pause();
		answers.push({ kind: 'reset' });
		await vi.advanceTimersByTimeAsync(5000);
		await flush();
		expect(state.rows).toEqual([]);
		state.dispose();
	});
	it('一時停止中でも、401 を受けたら止めた表示の行を捨てる', async () => {
		const { state, answers } = setup();
		answers.push({
			kind: 'ok',
			response: { entries: [conn('c:1', 100)], cursor: 'k1', more: false }
		});
		state.setVisible(true);
		await vi.advanceTimersByTimeAsync(0);
		await flush();
		state.pause();
		answers.push({ kind: 'unauthorized' });
		await vi.advanceTimersByTimeAsync(5000);
		await flush();
		expect(state.status).toBe('unauthorized');
		expect(state.rows).toEqual([]);
		expect(state.sources).toEqual([]);
		state.dispose();
	});
});
