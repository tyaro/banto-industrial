/**
 * 監査③（2026-08-12）是正のユニットテスト: `httpRequest`（`tagRegistryAdmin.ts`）
 * が 202 Accepted + `{ queued: true, ... }`（`queue_pending_registry_change`
 * の応答、収集稼働中に mutating エンドポイントを叩いたときに返る）を
 * `QueuedWhileRunningError` として弾き、`PlcConnection`/`Tag` 型として
 * 素通ししないことを固定する。
 *
 * **vitest 制約の回避について**（`apiKeysAdmin.test.ts` の doc comment 参照）:
 * `tagRegistryAdmin.ts` は `@banto/admin-core`（Svelte 5 rune を使う
 * `.svelte.ts` を推移的に import する）と `./setup`（`$lib/toast.svelte` を
 * import する）をトップレベルで import しており、そのままではこのリポジトリ
 * の最小 vitest 構成（`@sveltejs/vite-plugin-svelte` 無し、`$lib` エイリアス
 * 無し）で `ReferenceError: $state is not defined` / `Cannot find module
 * '$lib/toast.svelte'` になる。だが `@banto/admin-core` の値としての利用
 * 箇所は `getAuthProvider`/`ProviderError` の2つだけ、`./setup` は
 * `CSRF_HEADER` 定数だけなので、`vi.mock` でこの2モジュールを軽量な
 * フェイクに差し替えれば実モジュール（`$state`/`$lib` 依存の副作用）を
 * 一切評価せずに `tagRegistryAdmin.ts` 本体（`httpRequest` を含む）を
 * そのままロード・テストできる。`httpRequest` を抽出/リファクタしてまで
 * テスト可能にする必要はなかった。
 *
 * T19 S2-c2（UX-40）追記: `httpRequest` は変更系リクエストの直前で
 * `./deferredDelete.svelte`（同じく `$state` を使う `.svelte.ts`）の
 * `deferredDelete.flush()` を呼ぶようになった。同じ理由でこれも `vi.mock`
 * する - フェイクは `flush` の呼び出し回数を記録するだけの spy にして、
 * 下の「GET では呼ばない/非GETでは呼ぶ」テストで検証する。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';

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

const { deferredDeleteFlush } = vi.hoisted(() => ({ deferredDeleteFlush: vi.fn(async () => {}) }));
vi.mock('./deferredDelete.svelte', () => ({
	deferredDelete: { flush: deferredDeleteFlush }
}));

import {
	createPlcConnection,
	updatePlcConnection,
	createTag,
	updateTagsBatch,
	listTagsPaged,
	listTagGroupCounts,
	isQueuedWhileRunningError,
	QueuedWhileRunningError,
	type PlcConnection,
	type PlcConnectionInput,
	type Tag,
	type TagInput,
	type BatchTagUpdateRow
} from './tagRegistryAdmin';

const connectionInput: PlcConnectionInput = {
	name: 'plc1',
	protocol: 'modbus-tcp',
	host: '192.168.11.200',
	port: 502,
	unitId: 1,
	enabled: true,
	simulation: false,
	wordOrder: 'low_high'
};

const connectionResource: PlcConnection = { id: 1, ...connectionInput };

const tagInput: TagInput = {
	name: 'tag1',
	collectionGroupId: 1,
	address: 'D3000',
	dataType: 'i16',
	decimals: 0,
	enabled: true
};

const tagResource: Tag = {
	id: 1,
	name: 'tag1',
	collectionGroupId: 1,
	address: 'D3000',
	dataType: 'i16',
	stringLength: null,
	stringEncoding: 'utf8',
	rawLo: null,
	rawHi: null,
	engLo: null,
	engHi: null,
	unit: null,
	decimals: 0,
	thresholdH: null,
	thresholdHh: null,
	thresholdL: null,
	thresholdLl: null,
	enabled: true,
	writable: false,
	tagKind: 'plc',
	expression: null,
	retain: false,
	revision: 1
};

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

const queuedBody = {
	queued: true,
	message: 'テスト中は収集稼働中のためキューに投入しました',
	pending: { kind: 'create', resource: 'plc_connection' },
	status: { state: 'running' }
};

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('httpRequest の 202 (queued while running) 処理', () => {
	it('createPlcConnection は 202 queued 応答を QueuedWhileRunningError として reject する（PlcConnection として resolve しない）', async () => {
		mockFetchOnce({ status: 202, ok: true, body: queuedBody });
		await expect(createPlcConnection(connectionInput)).rejects.toSatisfy((err: unknown) => {
			expect(isQueuedWhileRunningError(err)).toBe(true);
			expect(err).toBeInstanceOf(QueuedWhileRunningError);
			expect((err as QueuedWhileRunningError).name).toBe('QueuedWhileRunningError');
			expect((err as Error).message).toBe(queuedBody.message);
			return true;
		});
	});

	it('updatePlcConnection も同様に QueuedWhileRunningError で reject する', async () => {
		mockFetchOnce({ status: 202, ok: true, body: queuedBody });
		await expect(updatePlcConnection(1, connectionInput)).rejects.toSatisfy((err: unknown) => {
			expect(isQueuedWhileRunningError(err)).toBe(true);
			return true;
		});
	});

	it('createTag も同様に QueuedWhileRunningError で reject する', async () => {
		mockFetchOnce({ status: 202, ok: true, body: queuedBody });
		await expect(createTag(tagInput)).rejects.toSatisfy((err: unknown) => {
			expect(isQueuedWhileRunningError(err)).toBe(true);
			return true;
		});
	});

	it('T18-3b: updateTagsBatch も同様に QueuedWhileRunningError で reject する', async () => {
		mockFetchOnce({ status: 202, ok: true, body: queuedBody });
		const rows: BatchTagUpdateRow[] = [{ id: 1, expectedRevision: 1, ...tagInput }];
		await expect(updateTagsBatch(rows, false)).rejects.toSatisfy((err: unknown) => {
			expect(isQueuedWhileRunningError(err)).toBe(true);
			return true;
		});
	});

	it('202 だが queued shape に合致しない body は ProviderError にフォールバックする（isQueuedWhileRunningError は false）', async () => {
		mockFetchOnce({ status: 202, ok: true, body: { unexpected: 'shape' } });
		await expect(createPlcConnection(connectionInput)).rejects.toSatisfy((err: unknown) => {
			expect(isQueuedWhileRunningError(err)).toBe(false);
			return true;
		});
	});
});

describe('httpRequest の通常成功パス（200/201）は 202 分岐追加の影響を受けない（回帰ガード）', () => {
	it('createPlcConnection は 201 で実体の PlcConnection を resolve する', async () => {
		mockFetchOnce({ status: 201, ok: true, body: connectionResource });
		const result = await createPlcConnection(connectionInput);
		expect(result).toEqual(connectionResource);
	});

	it('createTag は 200 で実体の Tag を resolve する', async () => {
		mockFetchOnce({ status: 200, ok: true, body: tagResource });
		const result = await createTag(tagInput);
		expect(result).toEqual(tagResource);
	});

	it('T18-3b: updateTagsBatch は 200 で BatchTagsUpdateResult を resolve する', async () => {
		const batchResult = {
			ok: true,
			dryRun: false,
			count: 1,
			errors: [],
			tags: [tagResource]
		};
		mockFetchOnce({ status: 200, ok: true, body: batchResult });
		const rows: BatchTagUpdateRow[] = [{ id: 1, expectedRevision: 1, ...tagInput }];
		const result = await updateTagsBatch(rows, false);
		expect(result).toEqual(batchResult);
	});
});

describe('T18-5a 第2段（docs/banto-hub-t18-design.md §4 決定6）: listTagsPaged / listTagGroupCounts の配線', () => {
	it('listTagsPaged は POST /api/tags/list に params をそのまま渡し ListResult<Tag> を resolve する', async () => {
		const listResult = { rows: [tagResource], totalCount: 1 };
		mockFetchOnce({ status: 200, ok: true, body: listResult });
		const params = {
			filters: [{ field: 'enabled', op: 'eq' as const, value: true }],
			sort: [],
			pagination: { offset: 0, limit: 50 }
		};

		const result = await listTagsPaged(params);

		expect(result).toEqual(listResult);
		const mockedFetch = fetch as unknown as ReturnType<typeof vi.fn>;
		const [url, init] = mockedFetch.mock.calls[0] as [string, RequestInit];
		expect(url).toBe('/api/tags/list');
		expect(init.method).toBe('POST');
		expect(JSON.parse(init.body as string)).toEqual(params);
	});

	it('listTagGroupCounts は GET /api/tags/group-counts で GroupTagCount[] を resolve する', async () => {
		const counts = [
			{ collectionGroupId: 1, tagCount: 3 },
			{ collectionGroupId: 2, tagCount: 1 }
		];
		mockFetchOnce({ status: 200, ok: true, body: counts });

		const result = await listTagGroupCounts();

		expect(result).toEqual(counts);
		const mockedFetch = fetch as unknown as ReturnType<typeof vi.fn>;
		const [url, init] = mockedFetch.mock.calls[0] as [string, RequestInit];
		expect(url).toBe('/api/tags/group-counts');
		expect(init.method).toBe('GET');
	});
});

describe('T19 S2-c2（UX-40、docs/banto-hub-t19-design.md §3.10）: httpRequest の deferredDelete.flush() フック', () => {
	beforeEach(() => {
		deferredDeleteFlush.mockClear();
	});

	it('GET では flush() を呼ばない（listTagGroupCounts）', async () => {
		mockFetchOnce({ status: 200, ok: true, body: [] });
		await listTagGroupCounts();
		expect(deferredDeleteFlush).not.toHaveBeenCalled();
	});

	it('POST（変更系）では fetch の前に flush() を呼ぶ（createTag）', async () => {
		mockFetchOnce({ status: 200, ok: true, body: tagResource });
		await createTag(tagInput);
		expect(deferredDeleteFlush).toHaveBeenCalledTimes(1);
	});

	it('PUT（変更系）でも flush() を呼ぶ（updatePlcConnection）', async () => {
		mockFetchOnce({ status: 200, ok: true, body: connectionResource });
		await updatePlcConnection(1, connectionInput);
		expect(deferredDeleteFlush).toHaveBeenCalledTimes(1);
	});
});
