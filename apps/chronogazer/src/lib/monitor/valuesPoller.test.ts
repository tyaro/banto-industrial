/**
 * 現在値ポーラー（`valuesPoller.svelte.ts`）のテスト。
 *
 * - [`applyValuesOutcome`] の表（結末ごとの反映）。
 * - ポーラーのクラス: 二重起動しない・停止で止まる・停止をまたいだ応答を捨てる・
 *   グループを切り替えたら値を持ち越さない・応答しない相手でもループが止まらない
 *   （上限で打ち切って失敗を数える）・連続 2 回で stale。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

import {
	INITIAL_VALUES_STATE,
	NOT_RUNNING_POLL_MS,
	ValuesPoller,
	applyValuesOutcome,
	nextPollDelayMs,
	type ValuesReadout,
	type ValuesState
} from './valuesPoller.svelte';

const READY_A: ValuesReadout = {
	state: 'ready',
	data: { 'tag:1': { value: 1, ptimeMs: 1, quality: 'good', lastGoodMs: 1 } }
};
const READY_B: ValuesReadout = {
	state: 'ready',
	data: { 'tag:2': { value: 2, ptimeMs: 2, quality: 'good', lastGoodMs: 2 } }
};

describe('applyValuesOutcome', () => {
	const shown: ValuesState = {
		phase: 'ready',
		values: READY_A.state === 'ready' ? READY_A.data : null,
		failures: 1,
		lastOkAt: 100
	};

	it.each<[string, Parameters<typeof applyValuesOutcome>[1], ValuesState]>([
		[
			'ok + ready: 差し替え・失敗 0・時刻',
			{ kind: 'ok', value: READY_B },
			{
				phase: 'ready',
				values: READY_B.state === 'ready' ? READY_B.data : null,
				failures: 0,
				lastOkAt: 999
			}
		],
		[
			'ok + notRunning: 値を捨てる・失敗 0',
			{ kind: 'ok', value: { state: 'notRunning' } },
			{ phase: 'notRunning', values: null, failures: 0, lastOkAt: 999 }
		],
		[
			'ok + unavailable: 失敗 +1・表示は残す',
			{ kind: 'ok', value: { state: 'unavailable' } },
			{ ...shown, failures: 2 }
		],
		[
			'failed: 失敗 +1・表示は残す',
			{ kind: 'failed', error: new Error('x') },
			{ ...shown, failures: 2 }
		],
		['timedOut: 失敗 +1・表示は残す', { kind: 'timedOut' }, { ...shown, failures: 2 }]
	])('%s', (_label, outcome, expected) => {
		expect(applyValuesOutcome(shown, outcome, 999)).toEqual(expected);
	});

	it('初期状態からの失敗は loading のまま数える（0 件に潰さない）', () => {
		expect(applyValuesOutcome(INITIAL_VALUES_STATE, { kind: 'timedOut' }, 1)).toEqual({
			...INITIAL_VALUES_STATE,
			failures: 1
		});
	});
});

describe('nextPollDelayMs（収集が動いていない間は間を空ける）', () => {
	it.each([
		['ready', 500, 500],
		['loading', 500, 500],
		['notRunning', 500, NOT_RUNNING_POLL_MS],
		['notRunning', 5000, 5000]
	] as const)('%s・周期 %i → %i', (phase, period, expected) => {
		expect(nextPollDelayMs(phase, period)).toBe(expected);
	});
});

/** 手で解決できる要求の列。 */
function deferredFetch() {
	const calls: {
		signal: AbortSignal;
		resolve: (r: ValuesReadout) => void;
		reject: (e: unknown) => void;
	}[] = [];
	const fetch = vi.fn(
		(signal: AbortSignal) =>
			new Promise<ValuesReadout>((resolve, reject) => {
				calls.push({ signal, resolve, reject });
			})
	);
	return { fetch, calls };
}

async function flush(): Promise<void> {
	for (let i = 0; i < 5; i++) await Promise.resolve();
}

describe('ValuesPoller', () => {
	beforeEach(() => {
		vi.useFakeTimers();
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	it('周期ごとに読み、同じ対象への start() ではループを増やさない', async () => {
		const { fetch, calls } = deferredFetch();
		const poller = new ValuesPoller({ fetch, timeoutMs: 4000, now: () => 7 });
		poller.start(1, 1000);
		poller.start(1, 1000);
		expect(fetch).toHaveBeenCalledTimes(1);
		calls[0].resolve(READY_A);
		await flush();
		expect(poller.state.phase).toBe('ready');
		expect(poller.state.lastOkAt).toBe(7);
		poller.start(1, 1000);
		await vi.advanceTimersByTimeAsync(1000);
		expect(fetch).toHaveBeenCalledTimes(2);
		poller.stop();
	});

	it('stop() のあとは次を予約しない。止める前に飛んでいた応答は採らない', async () => {
		const { fetch, calls } = deferredFetch();
		const poller = new ValuesPoller({ fetch, timeoutMs: 4000 });
		poller.start(1, 1000);
		poller.stop();
		calls[0].resolve(READY_A);
		await flush();
		expect(poller.state.phase).toBe('loading');
		await vi.advanceTimersByTimeAsync(5000);
		expect(fetch).toHaveBeenCalledTimes(1);
		expect(poller.running).toBe(false);
	});

	it('停止→同じ対象で再開: ループは 1 本だけ（古い応答が次のタイマを張らない）', async () => {
		const { fetch, calls } = deferredFetch();
		const poller = new ValuesPoller({ fetch, timeoutMs: 4000 });
		poller.start(1, 1000);
		poller.stop();
		poller.start(1, 1000);
		expect(fetch).toHaveBeenCalledTimes(2);
		calls[0].resolve(READY_B); // 古い世代
		calls[1].resolve(READY_A);
		await flush();
		expect(poller.state.values).toEqual(READY_A.state === 'ready' ? READY_A.data : null);
		await vi.advanceTimersByTimeAsync(1000);
		expect(fetch).toHaveBeenCalledTimes(3);
		poller.stop();
	});

	it('同じ対象での再開は値を残す（タブを隠して戻ったとき）', async () => {
		const { fetch, calls } = deferredFetch();
		const poller = new ValuesPoller({ fetch, timeoutMs: 4000 });
		poller.start(1, 1000);
		calls[0].resolve(READY_A);
		await flush();
		poller.stop();
		poller.start(1, 1000);
		expect(poller.state.phase).toBe('ready');
		poller.stop();
	});

	it('対象を切り替えると値を捨て、前の対象への飛行中の応答を採らない', async () => {
		const { fetch, calls } = deferredFetch();
		const poller = new ValuesPoller({ fetch, timeoutMs: 4000 });
		poller.start(1, 1000);
		calls[0].resolve(READY_A);
		await flush();
		await vi.advanceTimersByTimeAsync(1000);
		expect(fetch).toHaveBeenCalledTimes(2);
		// 2 本目（グループ 1 向け）が飛んでいる間に切り替える。
		poller.start(2, 1000);
		expect(poller.key).toBe(2);
		expect(poller.state).toEqual(INITIAL_VALUES_STATE);
		calls[1].resolve(READY_A);
		await flush();
		expect(poller.state.phase).toBe('loading');
		calls[2].resolve(READY_B);
		await flush();
		expect(poller.state.values).toEqual(READY_B.state === 'ready' ? READY_B.data : null);
		poller.stop();
	});

	it('応答しない相手: 上限で打ち切って signal を畳み、失敗を数え、ループは続く。2 回で stale', async () => {
		const { fetch, calls } = deferredFetch();
		const poller = new ValuesPoller({ fetch, timeoutMs: 4000 });
		poller.start(1, 1000);
		await vi.advanceTimersByTimeAsync(4000);
		expect(calls[0].signal.aborted).toBe(true);
		expect(poller.state.failures).toBe(1);
		expect(poller.stale).toBe(false);
		await vi.advanceTimersByTimeAsync(1000);
		expect(fetch).toHaveBeenCalledTimes(2);
		await vi.advanceTimersByTimeAsync(4000);
		expect(poller.state.failures).toBe(2);
		expect(poller.stale).toBe(true);
		// 打ち切った後に遅れて届いた応答は採らない。
		calls[0].resolve(READY_A);
		await flush();
		expect(poller.state.phase).toBe('loading');
		// 次が成功すれば戻る。
		await vi.advanceTimersByTimeAsync(1000);
		calls[2].resolve(READY_A);
		await flush();
		expect(poller.stale).toBe(false);
		expect(poller.state.phase).toBe('ready');
		poller.stop();
	});

	it('周期の差し替えは次の予約から効く', async () => {
		const { fetch, calls } = deferredFetch();
		const poller = new ValuesPoller({ fetch, timeoutMs: 4000 });
		poller.start(1, 5000);
		poller.start(1, 500);
		calls[0].resolve(READY_A);
		await flush();
		await vi.advanceTimersByTimeAsync(500);
		expect(fetch).toHaveBeenCalledTimes(2);
		poller.stop();
	});

	it('収集が動いていない間は NOT_RUNNING_POLL_MS 空けて読む', async () => {
		const { fetch, calls } = deferredFetch();
		const poller = new ValuesPoller({ fetch, timeoutMs: 4000 });
		poller.start(1, 500);
		calls[0].resolve({ state: 'notRunning' });
		await flush();
		expect(poller.state.phase).toBe('notRunning');
		await vi.advanceTimersByTimeAsync(500);
		expect(fetch).toHaveBeenCalledTimes(1);
		await vi.advanceTimersByTimeAsync(NOT_RUNNING_POLL_MS - 500);
		expect(fetch).toHaveBeenCalledTimes(2);
		poller.stop();
	});
});
