import { describe, expect, it, vi } from 'vitest';
import { purgeLoopbackRememberedToken } from './loopbackTokenPurge';
import { MemoryStorage, TOKEN_KEY } from './testing/hubHttp';

function setup(token: string | null = 'remembered') {
	const storage = new MemoryStorage();
	if (token) storage.setItem(TOKEN_KEY, token);
	return storage;
}

describe('purgeLoopbackRememberedToken（#644 PR5）', () => {
	it('loopback なら、そのトークンで logout を送って localStorage から消す', async () => {
		const storage = setup();
		const fetchFn = vi.fn(async () => new Response('{}', { status: 200 }));
		await purgeLoopbackRememberedToken({
			isLoopback: () => true,
			storage,
			fetchFn: fetchFn as unknown as typeof fetch
		});
		expect(fetchFn).toHaveBeenCalledTimes(1);
		const [url, init] = fetchFn.mock.calls[0] as unknown as [string, RequestInit];
		expect(url).toBe('/api/auth/logout');
		expect(init.method).toBe('POST');
		expect(init.headers).toMatchObject({
			Authorization: 'Bearer remembered',
			'X-Banto-Client': 'banto'
		});
		expect(storage.getItem(TOKEN_KEY)).toBeNull();
	});

	it('loopback でなければ何もしない（送らず、消さない）', async () => {
		const storage = setup();
		const fetchFn = vi.fn();
		await purgeLoopbackRememberedToken({
			isLoopback: () => false,
			storage,
			fetchFn: fetchFn as unknown as typeof fetch
		});
		expect(fetchFn).not.toHaveBeenCalled();
		expect(storage.getItem(TOKEN_KEY)).toBe('remembered');
	});

	it('logout が失敗（例外・401）しても消す', async () => {
		for (const fetchFn of [
			vi.fn(async () => {
				throw new TypeError('network');
			}),
			vi.fn(async () => new Response('{}', { status: 401 }))
		]) {
			const storage = setup();
			await purgeLoopbackRememberedToken({
				isLoopback: () => true,
				storage,
				fetchFn: fetchFn as unknown as typeof fetch
			});
			expect(fetchFn).toHaveBeenCalledTimes(1);
			expect(storage.getItem(TOKEN_KEY)).toBeNull();
		}
	});

	it('logout が応答しなくても 3000ms で打ち切って消す（#644 PR5）', async () => {
		vi.useFakeTimers();
		// AbortSignal.timeout の内部タイマーはフェイクできないので、フェイクの setTimeout で abort する signal に差し替える。
		const timeoutSpy = vi.spyOn(AbortSignal, 'timeout').mockImplementation((ms: number) => {
			const ac = new AbortController();
			setTimeout(() => ac.abort(new DOMException('timeout', 'TimeoutError')), ms);
			return ac.signal;
		});
		try {
			const storage = setup();
			const fetchFn = vi.fn(
				(_url: string, init: RequestInit) =>
					new Promise<Response>((_resolve, reject) => {
						init.signal?.addEventListener('abort', () => reject(init.signal?.reason));
					})
			);
			const done = purgeLoopbackRememberedToken({
				isLoopback: () => true,
				storage,
				fetchFn: fetchFn as unknown as typeof fetch
			});
			await vi.advanceTimersByTimeAsync(3000);
			await done;
			expect(timeoutSpy).toHaveBeenCalledWith(3000);
			expect(storage.getItem(TOKEN_KEY)).toBeNull();
		} finally {
			timeoutSpy.mockRestore();
			vi.useRealTimers();
		}
	});

	it('トークンが無い・localStorage が無いときは何も送らない', async () => {
		const fetchFn = vi.fn();
		await purgeLoopbackRememberedToken({
			isLoopback: () => true,
			storage: setup(null),
			fetchFn: fetchFn as unknown as typeof fetch
		});
		await purgeLoopbackRememberedToken({
			isLoopback: () => true,
			storage: undefined,
			fetchFn: fetchFn as unknown as typeof fetch
		});
		expect(fetchFn).not.toHaveBeenCalled();
	});
});
