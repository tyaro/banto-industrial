/**
 * `commissioning.ts` のユニットテスト。
 *
 * **vitest 制約の回避について**（`tagRegistryAdmin.test.ts` の doc comment
 * 参照）: `commissioning.ts` は `@banto/admin-core`（Svelte 5 rune を使う
 * `.svelte.ts` を推移的に import する）と `./setup`（`$lib/toast.svelte` を
 * import する）をトップレベルで import しており、そのままではこのリポジトリ
 * の最小 vitest 構成では読み込めない。値としての利用箇所は
 * `getAuthProvider`/`ProviderError`/`Identity`（型のみ）と `CSRF_HEADER`
 * だけなので、軽量なフェイクに差し替える。
 */
import { describe, expect, it, vi, afterEach } from 'vitest';

vi.mock('@banto/admin-core', () => ({
	getAuthProvider: () => ({ getToken: () => null }),
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
	COMMISSIONING_IDENTITY,
	fetchCommissioningStatusOrNull,
	getCommissioningStatus,
	lockDown,
	shouldBypassLoginForCommissioning,
	type CommissioningStatus
} from './commissioning';

function mockFetchOnce(response: { status: number; ok: boolean; body: unknown }): void {
	vi.stubGlobal(
		'fetch',
		vi.fn(async () => ({
			ok: response.ok,
			status: response.status,
			statusText: 'irrelevant',
			json: async () => response.body
		}))
	);
}

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('shouldBypassLoginForCommissioning（ルートガードの3分岐）', () => {
	it('試運転モード（lockedDown: false）ならログインを迂回する', () => {
		const status: CommissioningStatus = { lockedDown: false };
		expect(shouldBypassLoginForCommissioning(status)).toBe(true);
	});

	it('ロックダウン済み（lockedDown: true）なら通常どおりログインを要求する', () => {
		const status: CommissioningStatus = { lockedDown: true };
		expect(shouldBypassLoginForCommissioning(status)).toBe(false);
	});

	it('取得失敗（null）なら安全側に倒してログインを要求する', () => {
		expect(shouldBypassLoginForCommissioning(null)).toBe(false);
	});
});

describe('getCommissioningStatus / fetchCommissioningStatusOrNull', () => {
	it('200 で CommissioningStatus をそのまま resolve する', async () => {
		mockFetchOnce({ status: 200, ok: true, body: { lockedDown: false } });
		const result = await getCommissioningStatus();
		expect(result).toEqual({ lockedDown: false });

		const mockedFetch = fetch as unknown as ReturnType<typeof vi.fn>;
		const [url, init] = mockedFetch.mock.calls[0] as [string, RequestInit];
		expect(url).toBe('/api/commissioning/status');
		expect(init.method).toBe('GET');
		expect((init.headers as Record<string, string>)['X-Banto-Client']).toBe('banto');
	});

	it('fetch が例外を投げても fetchCommissioningStatusOrNull は throw せず null を返す', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => {
				throw new Error('network down');
			})
		);
		const result = await fetchCommissioningStatusOrNull();
		expect(result).toBeNull();
	});

	it('非2xx応答でも fetchCommissioningStatusOrNull は throw せず null を返す（安全側）', async () => {
		mockFetchOnce({ status: 500, ok: false, body: { kind: 'other', message: 'boom' } });
		const result = await fetchCommissioningStatusOrNull();
		expect(result).toBeNull();
	});

	it('getCommissioningStatus は非2xxで reject する（fetchCommissioningStatusOrNull との違い）', async () => {
		mockFetchOnce({ status: 500, ok: false, body: { kind: 'other', message: 'boom' } });
		await expect(getCommissioningStatus()).rejects.toThrow();
	});
});

describe('lockDown', () => {
	it('POST /api/commissioning/lock-down を叩き応答をそのまま resolve する', async () => {
		mockFetchOnce({ status: 200, ok: true, body: { lockedDown: true } });
		const result = await lockDown();
		expect(result).toEqual({ lockedDown: true });

		const mockedFetch = fetch as unknown as ReturnType<typeof vi.fn>;
		const [url, init] = mockedFetch.mock.calls[0] as [string, RequestInit];
		expect(url).toBe('/api/commissioning/lock-down');
		expect(init.method).toBe('POST');
	});

	it('admin アカウントが無い場合の validation エラーをそのまま reject する（詳細は body.field_errors 側）', async () => {
		const body = {
			kind: 'validation',
			field_errors: [{ field: 'lockDown', message: '管理者アカウントが1件も存在しません' }]
		};
		mockFetchOnce({ status: 400, ok: false, body });
		await expect(lockDown()).rejects.toSatisfy((err: unknown) => {
			const provider = err as { body?: typeof body };
			expect(provider.body).toEqual(body);
			return true;
		});
	});
});

describe('COMMISSIONING_IDENTITY', () => {
	it('サーバー側の synthetic_identity()（id: "commissioning", role: "admin"）と値が一致する', () => {
		expect(COMMISSIONING_IDENTITY).toEqual({
			id: 'commissioning',
			name: '試運転モード',
			role: 'admin'
		});
	});
});
