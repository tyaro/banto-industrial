import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createRangeLoader, isAbortError, type TrendFetchRequest } from './loader';
import type { TrendSeries } from './data';

const req = (startMs: number) => ({
	range: { startMs, endMs: startMs + 1000 },
	penIds: ['a'],
	bins: 10
});
const result = (v: number): Record<string, TrendSeries> => ({
	a: { bins: [{ tMs: 0, min: v, max: v }], binMs: 100 }
});

function deferred<T>() {
	let resolve!: (v: T) => void;
	let reject!: (e: unknown) => void;
	const promise = new Promise<T>((res, rej) => {
		resolve = res;
		reject = rej;
	});
	return { promise, resolve, reject };
}

const abortError = () => Object.assign(new Error('aborted'), { name: 'AbortError' });

describe('createRangeLoader', () => {
	beforeEach(() => vi.useFakeTimers());
	afterEach(() => vi.useRealTimers());

	it('続けて来た要求は最後のものだけを debounceMs 後に読む', async () => {
		const fetch = vi.fn(async (r: TrendFetchRequest) => result(r.range.startMs));
		const onResult = vi.fn();
		const loader = createRangeLoader({ fetch, debounceMs: 200, onResult });
		loader.request(req(1));
		await vi.advanceTimersByTimeAsync(100);
		loader.request(req(2));
		await vi.advanceTimersByTimeAsync(199);
		expect(fetch).not.toHaveBeenCalled();
		await vi.advanceTimersByTimeAsync(1);
		expect(fetch).toHaveBeenCalledTimes(1);
		expect(fetch.mock.calls[0][0].range.startMs).toBe(2);
		expect(fetch.mock.calls[0][0].signal).toBeInstanceOf(AbortSignal);
		expect(onResult).toHaveBeenCalledWith({ range: req(2).range, series: result(2) }, req(2));
	});

	it('既定の debounce は 200ms', async () => {
		const fetch = vi.fn(async () => result(0));
		const loader = createRangeLoader({ fetch, onResult: () => {} });
		loader.request(req(1));
		await vi.advanceTimersByTimeAsync(199);
		expect(fetch).not.toHaveBeenCalled();
		await vi.advanceTimersByTimeAsync(1);
		expect(fetch).toHaveBeenCalledTimes(1);
	});

	it('新しい要求で読み出し中のものを abort し、abort に従わない古い結果は捨てる', async () => {
		const pending: ReturnType<typeof deferred<Record<string, TrendSeries>>>[] = [];
		const signals: AbortSignal[] = [];
		const fetch = vi.fn((r: TrendFetchRequest) => {
			const d = deferred<Record<string, TrendSeries>>();
			pending.push(d);
			signals.push(r.signal);
			return d.promise;
		});
		const onResult = vi.fn();
		const onError = vi.fn();
		const loader = createRangeLoader({ fetch, debounceMs: 0, onResult, onError });
		loader.request(req(1));
		await vi.advanceTimersByTimeAsync(0);
		loader.request(req(2));
		expect(signals[0].aborted).toBe(true);
		await vi.advanceTimersByTimeAsync(0);
		// 古い fetch が abort を無視して後から返る
		pending[1].resolve(result(2));
		pending[0].resolve(result(1));
		await vi.advanceTimersByTimeAsync(0);
		expect(onResult).toHaveBeenCalledTimes(1);
		expect(onResult).toHaveBeenCalledWith({ range: req(2).range, series: result(2) }, req(2));
		expect(onError).not.toHaveBeenCalled();
	});

	it('AbortError と古い要求の失敗は onError に渡さない。今の要求の失敗は渡す', async () => {
		const pending: ReturnType<typeof deferred<Record<string, TrendSeries>>>[] = [];
		const fetch = vi.fn(() => {
			const d = deferred<Record<string, TrendSeries>>();
			pending.push(d);
			return d.promise;
		});
		const onError = vi.fn();
		const loader = createRangeLoader({ fetch, debounceMs: 0, onResult: () => {}, onError });
		loader.request(req(1));
		await vi.advanceTimersByTimeAsync(0);
		loader.request(req(2));
		await vi.advanceTimersByTimeAsync(0);
		pending[0].reject(new Error('old'));
		pending[1].reject(abortError());
		await vi.advanceTimersByTimeAsync(0);
		expect(onError).not.toHaveBeenCalled();

		loader.request(req(3));
		await vi.advanceTimersByTimeAsync(0);
		const boom = new Error('boom');
		pending[2].reject(boom);
		await vi.advanceTimersByTimeAsync(0);
		expect(onError).toHaveBeenCalledWith(boom, req(3));
	});

	it('fetch が同期で投げても onError に渡す', async () => {
		const boom = new Error('sync');
		const onError = vi.fn();
		const loader = createRangeLoader({
			fetch: () => {
				throw boom;
			},
			debounceMs: 0,
			onResult: () => {},
			onError
		});
		loader.request(req(1));
		await vi.advanceTimersByTimeAsync(0);
		expect(onError).toHaveBeenCalledWith(boom, req(1));
	});

	it('dispose で待ちと読み出し中を止め、以後の要求を無視する', async () => {
		const signals: AbortSignal[] = [];
		const d = deferred<Record<string, TrendSeries>>();
		const fetch = vi.fn((r: TrendFetchRequest) => {
			signals.push(r.signal);
			return d.promise;
		});
		const onResult = vi.fn();
		const loader = createRangeLoader({ fetch, debounceMs: 50, onResult });
		loader.request(req(1));
		await vi.advanceTimersByTimeAsync(50);
		loader.dispose();
		expect(signals[0].aborted).toBe(true);
		d.resolve(result(1));
		loader.request(req(2));
		await vi.advanceTimersByTimeAsync(100);
		expect(fetch).toHaveBeenCalledTimes(1);
		expect(onResult).not.toHaveBeenCalled();
	});

	it('dispose で debounce 中の要求も読まない', async () => {
		const fetch = vi.fn(async () => result(0));
		const loader = createRangeLoader({ fetch, debounceMs: 50, onResult: () => {} });
		loader.request(req(1));
		loader.dispose();
		await vi.advanceTimersByTimeAsync(100);
		expect(fetch).not.toHaveBeenCalled();
	});
});

describe('isAbortError', () => {
	it('name が AbortError のものだけ', () => {
		expect(isAbortError(abortError())).toBe(true);
		expect(isAbortError(new DOMException('x', 'AbortError'))).toBe(true);
		expect(isAbortError(new Error('x'))).toBe(false);
		expect(isAbortError(null)).toBe(false);
		expect(isAbortError('AbortError')).toBe(false);
	});
});
