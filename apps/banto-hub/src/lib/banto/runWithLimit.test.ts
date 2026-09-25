/**
 * `runWithLimit` のユニットテスト（#428。chronogazer の `hubAdmin.test.ts` の
 * 同名 describe と同じ中身）。「解決しない Promise」と `vi.useFakeTimers()` で
 * 無応答を作る - 即座に reject するモックでは、上限が無いときの欠陥
 * （`loading` が降りない）は再現できない。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { runWithLimit } from './runWithLimit';

const LIMIT_MS = 15000;

describe('runWithLimit', () => {
	afterEach(() => {
		vi.useRealTimers();
	});

	/** 解決も reject もしない往復（応答が返らないサーバー）。 */
	function neverSettles<T>(): {
		run: (signal: AbortSignal) => Promise<T>;
		signal: () => AbortSignal | null;
		resolveLate: (value: T) => void;
	} {
		let captured: AbortSignal | null = null;
		let settle: ((value: T) => void) | null = null;
		const pending = new Promise<T>((resolve) => {
			settle = resolve;
		});
		return {
			run: (signal) => {
				captured = signal;
				return pending;
			},
			signal: () => captured,
			resolveLate: (value) => settle?.(value)
		};
	}

	it('上限を過ぎても返ってこない往復は timedOut になり、signal が畳まれる', async () => {
		vi.useFakeTimers();
		const { run, signal } = neverSettles<void>();
		const pending = runWithLimit(run, LIMIT_MS);

		await vi.advanceTimersByTimeAsync(LIMIT_MS - 1);
		let settled = false;
		void pending.then(() => {
			settled = true;
		});
		await Promise.resolve();
		expect(settled).toBe(false);

		await vi.advanceTimersByTimeAsync(1);
		expect((await pending).kind).toBe('timedOut');
		expect(signal()?.aborted).toBe(true);
	});

	it('打ち切ったあとに遅れて解決しても ok にはしない', async () => {
		vi.useFakeTimers();
		const { run, resolveLate } = neverSettles<string>();
		const pending = runWithLimit(run, LIMIT_MS);
		await vi.advanceTimersByTimeAsync(LIMIT_MS);
		expect((await pending).kind).toBe('timedOut');

		resolveLate('遅れて届いた一覧');
		await vi.advanceTimersByTimeAsync(1000);
		expect((await pending).kind).toBe('timedOut');
	});

	it('上限内に解決すれば ok、reject は失敗の中身を運ぶ failed', async () => {
		vi.useFakeTimers();
		const ok = await runWithLimit(async () => 'list', LIMIT_MS);
		expect(ok).toEqual({ kind: 'ok', value: 'list' });

		const boom = new Error('boom');
		const failed = await runWithLimit(async () => {
			throw boom;
		}, LIMIT_MS);
		expect(failed).toEqual({ kind: 'failed', error: boom });
	});
});
