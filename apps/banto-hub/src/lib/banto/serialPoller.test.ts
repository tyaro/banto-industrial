import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createSerialPoller, PollTimeoutError } from './serialPoller';

/** 外から決着を操作できる Promise。 */
function deferred<T>() {
	let resolve!: (value: T) => void;
	let reject!: (error: unknown) => void;
	const promise = new Promise<T>((res, rej) => {
		resolve = res;
		reject = rej;
	});
	return { promise, resolve, reject };
}

describe('createSerialPoller', () => {
	beforeEach(() => {
		vi.useFakeTimers();
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	it('作った直後に 1 回目を走らせ、決着の intervalMs 後に次を走らせる', async () => {
		const run = vi.fn(async () => 1);
		const onResult = vi.fn();
		const poller = createSerialPoller({ intervalMs: 3000, run, onResult });
		expect(run).toHaveBeenCalledTimes(1);
		await vi.advanceTimersByTimeAsync(0);
		expect(onResult).toHaveBeenCalledWith(1);
		await vi.advanceTimersByTimeAsync(2999);
		expect(run).toHaveBeenCalledTimes(1);
		await vi.advanceTimersByTimeAsync(1);
		expect(run).toHaveBeenCalledTimes(2);
		poller.stop();
	});

	it('run が遅いあいだは重ならず、決着してから次を予約する', async () => {
		const pending = [deferred<number>(), deferred<number>()];
		let call = 0;
		const run = vi.fn(() => pending[call++].promise);
		// 期限なし（timeoutMs: 0）で、重ならないことだけを確かめる。
		const poller = createSerialPoller({ intervalMs: 3000, timeoutMs: 0, run });
		// 3 秒の周期を何度またいでも、1 回目が未決着なら 2 回目は始まらない。
		await vi.advanceTimersByTimeAsync(30_000);
		expect(run).toHaveBeenCalledTimes(1);
		pending[0].resolve(1);
		await vi.advanceTimersByTimeAsync(0);
		// 決着した時点ではまだ予約だけ（待ちの前に次は走らない）。
		expect(run).toHaveBeenCalledTimes(1);
		await vi.advanceTimersByTimeAsync(3000);
		expect(run).toHaveBeenCalledTimes(2);
		poller.stop();
	});

	it('失敗しても次の回を予約し、onError と onSettled を呼ぶ', async () => {
		const run = vi.fn<(signal: AbortSignal) => Promise<number>>();
		run.mockRejectedValueOnce(new Error('boom')).mockResolvedValueOnce(2);
		const onError = vi.fn();
		const onResult = vi.fn();
		const onSettled = vi.fn();
		const poller = createSerialPoller({ intervalMs: 1000, run, onError, onResult, onSettled });
		await vi.advanceTimersByTimeAsync(0);
		expect(onError).toHaveBeenCalledTimes(1);
		expect(onSettled).toHaveBeenCalledTimes(1);
		await vi.advanceTimersByTimeAsync(1000);
		expect(onResult).toHaveBeenCalledWith(2);
		expect(onSettled).toHaveBeenCalledTimes(2);
		poller.stop();
	});

	it('stop() は進行中の要求を中断し、以降の回を走らせない', async () => {
		const seen: AbortSignal[] = [];
		const pending = deferred<number>();
		const run = vi.fn((signal: AbortSignal) => {
			seen.push(signal);
			return pending.promise;
		});
		const poller = createSerialPoller({ intervalMs: 1000, run });
		expect(seen[0].aborted).toBe(false);
		poller.stop();
		expect(seen[0].aborted).toBe(true);
		await vi.advanceTimersByTimeAsync(10_000);
		expect(run).toHaveBeenCalledTimes(1);
	});

	it('待ちの間に stop() すると予約を取り消す', async () => {
		const run = vi.fn(async () => 1);
		const poller = createSerialPoller({ intervalMs: 1000, run });
		await vi.advanceTimersByTimeAsync(0);
		poller.stop();
		await vi.advanceTimersByTimeAsync(10_000);
		expect(run).toHaveBeenCalledTimes(1);
		poller.stop(); // 二重でもよい
	});

	it('stop() の後に決着した古い結果・エラーは捨てる', async () => {
		const first = deferred<string>();
		const run = vi.fn(() => first.promise);
		const onResult = vi.fn();
		const onError = vi.fn();
		const onSettled = vi.fn();
		const poller = createSerialPoller({ intervalMs: 1000, run, onResult, onError, onSettled });
		poller.stop();
		first.resolve('stale');
		await vi.advanceTimersByTimeAsync(0);
		expect(onResult).not.toHaveBeenCalled();
		expect(onSettled).not.toHaveBeenCalled();

		const second = deferred<string>();
		const run2 = vi.fn(() => second.promise);
		const poller2 = createSerialPoller({
			intervalMs: 1000,
			run: run2,
			onResult,
			onError,
			onSettled
		});
		poller2.stop();
		second.reject(new DOMException('aborted', 'AbortError'));
		await vi.advanceTimersByTimeAsync(0);
		expect(onError).not.toHaveBeenCalled();
		expect(onSettled).not.toHaveBeenCalled();
	});

	it('作り直した新しいポーラーの結果は、止めた古い回に上書きされない', async () => {
		const oldCall = deferred<string>();
		const latest: string[] = [];
		const oldPoller = createSerialPoller({
			intervalMs: 1000,
			run: () => oldCall.promise,
			onResult: (v) => latest.push(v)
		});
		oldPoller.stop();
		const newPoller = createSerialPoller({
			intervalMs: 1000,
			run: async () => 'new',
			onResult: (v) => latest.push(v)
		});
		await vi.advanceTimersByTimeAsync(0);
		oldCall.resolve('old');
		await vi.advanceTimersByTimeAsync(0);
		expect(latest).toEqual(['new']);
		newPoller.stop();
	});

	describe('1 回ごとの期限（timeoutMs）', () => {
		it('決着しない run は期限で中断・onError(時間切れ)・onSettled し、intervalMs 後に次を走らせる', async () => {
			const seen: AbortSignal[] = [];
			const run = vi.fn((signal: AbortSignal) => {
				seen.push(signal);
				return new Promise<number>(() => {});
			});
			const onError = vi.fn();
			const onSettled = vi.fn();
			const poller = createSerialPoller({
				intervalMs: 3000,
				timeoutMs: 5000,
				run,
				onError,
				onSettled
			});
			await vi.advanceTimersByTimeAsync(4999);
			expect(onError).not.toHaveBeenCalled();
			await vi.advanceTimersByTimeAsync(1);
			expect(seen[0].aborted).toBe(true);
			expect(onError).toHaveBeenCalledTimes(1);
			expect(onError.mock.calls[0][0]).toBeInstanceOf(PollTimeoutError);
			expect((onError.mock.calls[0][0] as Error).name).toBe('TimeoutError');
			expect(onSettled).toHaveBeenCalledTimes(1);
			expect(run).toHaveBeenCalledTimes(1);
			await vi.advanceTimersByTimeAsync(2999);
			expect(run).toHaveBeenCalledTimes(1);
			await vi.advanceTimersByTimeAsync(1);
			expect(run).toHaveBeenCalledTimes(2);
			poller.stop();
		});

		it('期限前に決着したら期限タイマーは消え、余計なエラーは出ない', async () => {
			const run = vi.fn(async () => 1);
			const onError = vi.fn();
			const onResult = vi.fn();
			const poller = createSerialPoller({
				intervalMs: 100_000,
				timeoutMs: 5000,
				run,
				onResult,
				onError
			});
			await vi.advanceTimersByTimeAsync(0);
			expect(onResult).toHaveBeenCalledWith(1);
			await vi.advanceTimersByTimeAsync(20_000);
			expect(onError).not.toHaveBeenCalled();
			expect(vi.getTimerCount()).toBe(1); // 次回予約だけ
			poller.stop();
		});

		it('期限切れの後に遅れて決着した結果・エラーは捨てる', async () => {
			const first = deferred<number>();
			const run = vi.fn<(signal: AbortSignal) => Promise<number>>();
			run.mockReturnValueOnce(first.promise).mockReturnValue(new Promise(() => {}));
			const onResult = vi.fn();
			const onError = vi.fn();
			const onSettled = vi.fn();
			const poller = createSerialPoller({
				intervalMs: 1000,
				timeoutMs: 2000,
				run,
				onResult,
				onError,
				onSettled
			});
			await vi.advanceTimersByTimeAsync(2000);
			expect(onError).toHaveBeenCalledTimes(1);
			first.resolve(99);
			await vi.advanceTimersByTimeAsync(0);
			expect(onResult).not.toHaveBeenCalled();
			expect(onError).toHaveBeenCalledTimes(1);
			expect(onSettled).toHaveBeenCalledTimes(1);
			// 重ならず、次の回は 1 回だけ予約されている。
			await vi.advanceTimersByTimeAsync(1000);
			expect(run).toHaveBeenCalledTimes(2);
			poller.stop();
		});

		it('期限待ちの間に stop() すると、エラー通知も次の回も起きない', async () => {
			const run = vi.fn((_signal: AbortSignal) => new Promise<number>(() => {}));
			const onError = vi.fn();
			const onSettled = vi.fn();
			const poller = createSerialPoller({
				intervalMs: 1000,
				timeoutMs: 2000,
				run,
				onError,
				onSettled
			});
			await vi.advanceTimersByTimeAsync(1000);
			poller.stop();
			expect(vi.getTimerCount()).toBe(0);
			await vi.advanceTimersByTimeAsync(60_000);
			expect(onError).not.toHaveBeenCalled();
			expect(onSettled).not.toHaveBeenCalled();
			expect(run).toHaveBeenCalledTimes(1);
		});

		it('既定の期限は 10 秒', async () => {
			const onError = vi.fn();
			const poller = createSerialPoller({
				intervalMs: 1000,
				run: () => new Promise<number>(() => {}),
				onError
			});
			await vi.advanceTimersByTimeAsync(9999);
			expect(onError).not.toHaveBeenCalled();
			await vi.advanceTimersByTimeAsync(1);
			expect(onError).toHaveBeenCalledTimes(1);
			poller.stop();
		});
	});
});
