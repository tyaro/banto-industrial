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

import {
	createPlcConnection,
	updatePlcConnection,
	createTag,
	isQueuedWhileRunningError,
	QueuedWhileRunningError,
	type PlcConnection,
	type PlcConnectionInput,
	type Tag,
	type TagInput
} from './tagRegistryAdmin';

const connectionInput: PlcConnectionInput = {
	name: 'plc1',
	protocol: 'modbus-tcp',
	host: '192.168.11.200',
	port: 502,
	unitId: 1,
	enabled: true,
	simulation: false
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
});
