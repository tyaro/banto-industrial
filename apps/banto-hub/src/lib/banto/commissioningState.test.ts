/**
 * `commissioningState.svelte.ts` のユニットテスト。
 *
 * 守りたいこと（2026-10-04 オーナー指示）: 「サーバーが試運転中か」はサーバーの
 * `GET /api/commissioning/status` から取り、セッションの種別には依らない。取得に
 * 失敗したら「分からない」（`null`）で、`serverCommissioning` は false（欄を出さない側）。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';

vi.mock('@banto/admin-core', () => ({
	getAuthProvider: () => ({ getToken: () => null }),
	ProviderError: class ProviderError extends Error {}
}));
vi.mock('./setup', () => ({ CSRF_HEADER: { 'X-Banto-Client': 'banto' } }));

import { commissioningState } from './commissioningState.svelte';

function mockFetch(response: { status: number; ok: boolean; body: unknown } | 'throw'): void {
	vi.stubGlobal(
		'fetch',
		vi.fn(async () => {
			if (response === 'throw') throw new TypeError('Failed to fetch');
			return {
				ok: response.ok,
				status: response.status,
				statusText: 'irrelevant',
				json: async () => response.body
			};
		})
	);
}

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('commissioningState', () => {
	it('未ロックダウンなら serverCommissioning は true（セッションの種別には依らない）', async () => {
		mockFetch({ status: 200, ok: true, body: { lockedDown: false } });
		expect(await commissioningState.refresh()).toBe(false);
		expect(commissioningState.lockedDown).toBe(false);
		expect(commissioningState.serverCommissioning).toBe(true);
	});

	it('ロックダウン済みなら false', async () => {
		mockFetch({ status: 200, ok: true, body: { lockedDown: true } });
		expect(await commissioningState.refresh()).toBe(true);
		expect(commissioningState.serverCommissioning).toBe(false);
	});

	it('取得に失敗したら null で、serverCommissioning は false（分からないは出さない側）', async () => {
		mockFetch({ status: 200, ok: true, body: { lockedDown: false } });
		await commissioningState.refresh();
		mockFetch('throw');
		expect(await commissioningState.refresh()).toBeNull();
		expect(commissioningState.lockedDown).toBeNull();
		expect(commissioningState.serverCommissioning).toBe(false);
	});

	it('markLockedDown は問い合わせずにロックダウン済みにする', async () => {
		mockFetch({ status: 200, ok: true, body: { lockedDown: false } });
		await commissioningState.refresh();
		commissioningState.markLockedDown();
		expect(commissioningState.serverCommissioning).toBe(false);
		expect(fetch).toHaveBeenCalledTimes(1);
	});
});
