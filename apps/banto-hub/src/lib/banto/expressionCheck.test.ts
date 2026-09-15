/**
 * `expressionCheck.ts` のユニットテスト。`apiKeysAdmin.test.ts`/
 * `tagRegistryAdmin.test.ts` の doc comment にあるとおり、`@banto/admin-core`
 * （`$state` を使う `.svelte.ts` を推移的に import する）と `./setup`
 * （`$lib/toast.svelte` を import する）は `vi.mock` で軽量フェイクに
 * 差し替える - このリポジトリの最小 vitest 構成（`@sveltejs/vite-plugin-svelte`
 * 無し）では実モジュールをロードすると `ReferenceError: $state is not
 * defined` になるため。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@banto/admin-core', () => ({
	getAuthProvider: () => ({ getToken: () => 'test-token' }),
	ProviderError: class ProviderError extends Error {
		body: unknown;
		constructor(body: unknown) {
			super(
				typeof body === 'object' && body !== null && 'message' in body
					? String((body as { message: unknown }).message)
					: 'provider error'
			);
			this.name = 'ProviderError';
			this.body = body;
		}
	}
}));

vi.mock('./setup', () => ({
	CSRF_HEADER: { 'X-Banto-Client': 'banto' }
}));

import {
	checkExpression,
	ExpressionCheckController,
	shouldCheckExpression,
	type ExpressionCheckResult
} from './expressionCheck';
import { ProviderError } from '@banto/admin-core';

function okResult(overrides: Partial<ExpressionCheckResult> = {}): ExpressionCheckResult {
	return {
		ok: true,
		resultType: 'num',
		refs: [],
		preview: { value: 1, evaluated: true, reason: null },
		error: null,
		...overrides
	};
}

// ---------------------------------------------------------------------------
// shouldCheckExpression
// ---------------------------------------------------------------------------

describe('shouldCheckExpression', () => {
	it('computed かつ非空なら true', () => {
		expect(shouldCheckExpression('computed', 'a + 1')).toBe(true);
	});

	it('computed 以外は常に false', () => {
		expect(shouldCheckExpression('plc', 'a + 1')).toBe(false);
		expect(shouldCheckExpression('internal', 'a + 1')).toBe(false);
	});

	it('前後空白のみの式は false（未入力扱い）', () => {
		expect(shouldCheckExpression('computed', '   ')).toBe(false);
		expect(shouldCheckExpression('computed', '')).toBe(false);
	});
});

// ---------------------------------------------------------------------------
// checkExpression（fetch モック）
// ---------------------------------------------------------------------------

describe('checkExpression', () => {
	const originalFetch = globalThis.fetch;

	afterEach(() => {
		globalThis.fetch = originalFetch;
	});

	it('200 応答をそのまま ExpressionCheckResult として返す', async () => {
		const body = okResult({ resultType: 'bool' });
		globalThis.fetch = vi.fn(
			async () => new Response(JSON.stringify(body), { status: 200 })
		) as never;

		const result = await checkExpression('a > b', null);
		expect(result).toEqual(body);
	});

	it('リクエストに Authorization/CSRF ヘッダと body を付ける', async () => {
		let capturedInit: RequestInit | undefined;
		globalThis.fetch = vi.fn(async (_url: string, init?: RequestInit) => {
			capturedInit = init;
			return new Response(JSON.stringify(okResult()), { status: 200 });
		}) as never;

		await checkExpression('mem.x.a + 1', 'calc.y.z');

		expect(capturedInit?.method).toBe('POST');
		const headers = capturedInit?.headers as Record<string, string>;
		expect(headers.Authorization).toBe('Bearer test-token');
		expect(headers['X-Banto-Client']).toBe('banto');
		expect(JSON.parse(capturedInit?.body as string)).toEqual({
			expression: 'mem.x.a + 1',
			externalName: 'calc.y.z'
		});
	});

	it('非 2xx はエラー body を ProviderError として投げる', async () => {
		globalThis.fetch = vi.fn(
			async () =>
				new Response(JSON.stringify({ kind: 'forbidden', message: '権限がありません' }), {
					status: 403
				})
		) as never;

		await expect(checkExpression('1', null)).rejects.toBeInstanceOf(ProviderError);
	});

	it('fetch 自体が失敗したらネットワークエラーの ProviderError を投げる', async () => {
		globalThis.fetch = vi.fn(async () => {
			throw new TypeError('network down');
		}) as never;

		await expect(checkExpression('1', null)).rejects.toBeInstanceOf(ProviderError);
	});
});

// ---------------------------------------------------------------------------
// ExpressionCheckController
// ---------------------------------------------------------------------------

describe('ExpressionCheckController', () => {
	beforeEach(() => {
		vi.useFakeTimers();
	});

	afterEach(() => {
		vi.useRealTimers();
	});

	it('300ms 経つまでは run を呼ばない（debounce）', () => {
		const run = vi.fn(async () => okResult());
		const onResult = vi.fn();
		const controller = new ExpressionCheckController({ run, onResult });

		controller.scheduleCheck('a', null);
		vi.advanceTimersByTime(299);
		expect(run).not.toHaveBeenCalled();
	});

	it('連打しても最後の呼び出しから300ms後に1回だけ run が呼ばれる', async () => {
		const run = vi.fn(async () => okResult());
		const onResult = vi.fn();
		const controller = new ExpressionCheckController({ run, onResult });

		controller.scheduleCheck('a', null);
		vi.advanceTimersByTime(100);
		controller.scheduleCheck('ab', null);
		vi.advanceTimersByTime(100);
		controller.scheduleCheck('abc', null);

		await vi.advanceTimersByTimeAsync(300);

		expect(run).toHaveBeenCalledTimes(1);
		expect(run).toHaveBeenCalledWith('abc', null);
		expect(onResult).toHaveBeenCalledTimes(1);
	});

	it('空文字（前後空白のみ含む）は run を呼ばず即座に onResult(null)', () => {
		const run = vi.fn(async () => okResult());
		const onResult = vi.fn();
		const controller = new ExpressionCheckController({ run, onResult });

		controller.scheduleCheck('   ', null);

		expect(run).not.toHaveBeenCalled();
		expect(onResult).toHaveBeenCalledWith(null);
	});

	it('IME 変換中（compositionstart 後）は scheduleCheck してもタイマーが動かない', async () => {
		const run = vi.fn(async () => okResult());
		const onResult = vi.fn();
		const controller = new ExpressionCheckController({ run, onResult });

		controller.onCompositionStart();
		controller.scheduleCheck('あ', null);
		await vi.advanceTimersByTimeAsync(1000);

		expect(run).not.toHaveBeenCalled();
	});

	it('compositionend で確定内容を改めてスケジュールする', async () => {
		const run = vi.fn(async () => okResult());
		const onResult = vi.fn();
		const controller = new ExpressionCheckController({ run, onResult });

		controller.onCompositionStart();
		controller.scheduleCheck('あ', null);
		controller.onCompositionEnd('あいう', null);
		await vi.advanceTimersByTimeAsync(300);

		expect(run).toHaveBeenCalledTimes(1);
		expect(run).toHaveBeenCalledWith('あいう', null);
	});

	it('古いリクエストの応答が後から返ってきても新しい結果を上書きしない', async () => {
		const results: Record<string, ExpressionCheckResult> = {
			first: okResult({ resultType: 'num' }),
			second: okResult({ resultType: 'bool' })
		};
		// "first" の応答は "second" より遅れて解決する。
		let resolveFirst: (v: ExpressionCheckResult) => void = () => {};
		const firstPromise = new Promise<ExpressionCheckResult>((resolve) => {
			resolveFirst = resolve;
		});
		const run = vi
			.fn()
			.mockImplementationOnce(async () => firstPromise)
			.mockImplementationOnce(async () => results.second);
		const onResult = vi.fn();
		const controller = new ExpressionCheckController({ run, onResult, delayMs: 10 });

		controller.scheduleCheck('first-expr', null);
		await vi.advanceTimersByTimeAsync(10);
		controller.scheduleCheck('second-expr', null);
		await vi.advanceTimersByTimeAsync(10);

		// "second" (later request) resolves first.
		expect(onResult).toHaveBeenCalledTimes(1);
		expect(onResult).toHaveBeenCalledWith(results.second);

		// Now let the stale "first" response arrive - must not overwrite.
		resolveFirst(results.first);
		await Promise.resolve();
		await Promise.resolve();
		expect(onResult).toHaveBeenCalledTimes(1);
	});

	it('run が reject したら onError を呼ぶ（onResult は呼ばない）', async () => {
		const err = new Error('boom');
		const run = vi.fn(async () => {
			throw err;
		});
		const onResult = vi.fn();
		const onError = vi.fn();
		const controller = new ExpressionCheckController({ run, onResult, onError });

		controller.scheduleCheck('a', null);
		await vi.advanceTimersByTimeAsync(300);

		expect(onError).toHaveBeenCalledWith(err);
		expect(onResult).not.toHaveBeenCalled();
	});

	it('cancel() で保留中のタイマーを止める', async () => {
		const run = vi.fn(async () => okResult());
		const onResult = vi.fn();
		const controller = new ExpressionCheckController({ run, onResult });

		controller.scheduleCheck('a', null);
		controller.cancel();
		await vi.advanceTimersByTimeAsync(1000);

		expect(run).not.toHaveBeenCalled();
	});

	it('onStart は run 呼び出し直前に呼ばれる', async () => {
		const order: string[] = [];
		const run = vi.fn(async () => {
			order.push('run');
			return okResult();
		});
		const onStart = vi.fn(() => order.push('start'));
		const onResult = vi.fn();
		const controller = new ExpressionCheckController({ run, onStart, onResult });

		controller.scheduleCheck('a', null);
		await vi.advanceTimersByTimeAsync(300);

		expect(order).toEqual(['start', 'run']);
	});
});
