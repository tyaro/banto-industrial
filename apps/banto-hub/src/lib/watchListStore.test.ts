import { describe, expect, it, vi } from 'vitest';
import type { UiSettingsProvider } from '@banto/admin-core';
import { MemoryStorage } from './banto/testing/hubHttp';

vi.mock('./banto/setup', () => ({ getUiSettings: () => undefined }));

import { createWatchListStore } from './watchListStore';
import { serializeWatchList } from './watchList';

const LOCAL_KEY = 'banto.watch.v1.account%3A1';

/**
 * サーバーの代わり。`deferGet` なら GET は `release()`（または `fail()`）まで返らない
 * （読み込み中の操作を再現する）。
 */
function fakeProvider(initial?: string, opts: { deferGet?: boolean } = {}) {
	const values = new Map<string, string>();
	if (initial !== undefined) values.set('hub.watch.v1', initial);
	const set = vi.fn(async (key: string, value: string) => {
		values.set(key, value);
	});
	let release: () => void = () => undefined;
	let fail: () => void = () => undefined;
	const get = vi.fn((key: string) => {
		if (!opts.deferGet) return Promise.resolve(values.get(key) ?? null);
		return new Promise<string | null>((resolve, reject) => {
			release = () => resolve(values.get(key) ?? null);
			fail = () => reject(new Error('offline'));
		});
	});
	const provider = { get, set } as unknown as UiSettingsProvider;
	return { provider, values, set, release: () => release(), fail: () => fail() };
}

async function settle(): Promise<void> {
	for (let i = 0; i < 10; i++) await Promise.resolve();
}

describe('createWatchListStore', () => {
	it('最初の描画には写しを使う（壊れていれば空）', () => {
		const storage = new MemoryStorage();
		const server = fakeProvider();
		const deps = { provider: server.provider, storage: () => storage, owner: () => 'account:1' };
		storage.setItem(LOCAL_KEY, serializeWatchList([4, 5]));
		expect(createWatchListStore(deps).initial).toEqual([4, 5]);
		expect(createWatchListStore(deps).list).toEqual([4, 5]);
		storage.setItem(LOCAL_KEY, '{broken');
		expect(createWatchListStore(deps).initial).toEqual([]);
	});

	it('確定した後の操作は写しとサーバーへ即送る', async () => {
		const storage = new MemoryStorage();
		const server = fakeProvider();
		const store = createWatchListStore({
			provider: server.provider,
			storage: () => storage,
			owner: () => 'account:1'
		});
		expect(await store.load()).toBe(true);
		expect(server.set).not.toHaveBeenCalled();
		store.apply({ kind: 'add', ids: [1, 2] });
		await settle();
		expect(server.set).toHaveBeenCalledTimes(1);
		expect(server.values.get('hub.watch.v1')).toBe(serializeWatchList([1, 2]));
		expect(storage.getItem(LOCAL_KEY)).toBe(serializeWatchList([1, 2]));
	});

	it('サーバーの値を一覧にし、送り返さない', async () => {
		const server = fakeProvider(serializeWatchList([7, 8]));
		const store = createWatchListStore({
			provider: server.provider,
			storage: () => new MemoryStorage(),
			owner: () => 'account:1'
		});
		expect(await store.load()).toBe(true);
		expect(store.list).toEqual([7, 8]);
		await settle();
		expect(server.set).not.toHaveBeenCalled();
	});

	it('(a) 写しが無く読み込み中に足したら、サーバーの一覧に足して保存する（確定前は送らない）', async () => {
		const server = fakeProvider(serializeWatchList([1, 2]), { deferGet: true });
		const store = createWatchListStore({
			provider: server.provider,
			storage: () => new MemoryStorage(),
			owner: () => 'account:1'
		});
		const loading = store.load();
		const result = store.apply({ kind: 'add', ids: [3] });
		expect(result.added).toBe(1);
		expect(store.list).toEqual([3]);
		await settle();
		expect(server.set).not.toHaveBeenCalled();
		server.release();
		expect(await loading).toBe(true);
		expect(store.list).toEqual([1, 2, 3]);
		await settle();
		expect(server.set).toHaveBeenCalledTimes(1);
		expect(server.values.get('hub.watch.v1')).toBe(serializeWatchList([1, 2, 3]));
	});

	it('(b) 確定前の消えた id の除去は何もせず、サーバーの一覧を上書きしない', async () => {
		const storage = new MemoryStorage();
		storage.setItem(LOCAL_KEY, serializeWatchList([1, 2]));
		const server = fakeProvider(serializeWatchList([2, 3]), { deferGet: true });
		const store = createWatchListStore({
			provider: server.provider,
			storage: () => storage,
			owner: () => 'account:1'
		});
		const loading = store.load();
		// タグ一覧が先に読めた（タグ 1 は別の所で消された）。
		store.apply({ kind: 'prune', existing: new Set([2, 3]) });
		expect(store.list).toEqual([1, 2]);
		server.release();
		expect(await loading).toBe(true);
		expect(store.list).toEqual([2, 3]);
		// 確定した後の除去は当たる（消えていないので変わらない）。
		store.apply({ kind: 'prune', existing: new Set([2, 3]) });
		await settle();
		expect(store.list).toEqual([2, 3]);
		expect(server.set).not.toHaveBeenCalled();
		expect(server.values.get('hub.watch.v1')).toBe(serializeWatchList([2, 3]));
		expect(storage.getItem(LOCAL_KEY)).toBe(serializeWatchList([2, 3]));
	});

	it('溜めた外す・すべて外すもサーバーの基準に当て直す', async () => {
		const server = fakeProvider(serializeWatchList([1, 2, 3]), { deferGet: true });
		const store = createWatchListStore({
			provider: server.provider,
			storage: () => new MemoryStorage(),
			owner: () => 'account:1'
		});
		const loading = store.load();
		store.apply({ kind: 'clear' });
		store.apply({ kind: 'add', ids: [4, 5] });
		store.apply({ kind: 'remove', id: 4 });
		server.release();
		await loading;
		expect(store.list).toEqual([5]);
		await settle();
		expect(server.values.get('hub.watch.v1')).toBe(serializeWatchList([5]));
	});

	it('読みが失敗したら、溜めた操作を表示に残したまま保存しない。読み直せば当て直す', async () => {
		const server = fakeProvider(serializeWatchList([1, 2]), { deferGet: true });
		const store = createWatchListStore({
			provider: server.provider,
			storage: () => new MemoryStorage(),
			owner: () => 'account:1'
		});
		const first = store.load();
		store.apply({ kind: 'add', ids: [3] });
		server.fail();
		expect(await first).toBe(false);
		expect(store.confirmed).toBe(false);
		expect(store.list).toEqual([3]);
		store.apply({ kind: 'add', ids: [4] });
		await settle();
		expect(server.set).not.toHaveBeenCalled();
		const second = store.load();
		server.release();
		expect(await second).toBe(true);
		expect(store.list).toEqual([1, 2, 3, 4]);
		await settle();
		expect(server.values.get('hub.watch.v1')).toBe(serializeWatchList([1, 2, 3, 4]));
	});

	it('保存値が無く一覧も空なら送らない', async () => {
		const server = fakeProvider();
		const store = createWatchListStore({
			provider: server.provider,
			storage: () => new MemoryStorage(),
			owner: () => 'account:1'
		});
		expect(await store.load()).toBe(true);
		await settle();
		expect(server.set).not.toHaveBeenCalled();
	});

	it('持ち主が分からなければ写しを読まず書かない', async () => {
		const storage = new MemoryStorage();
		const setItem = vi.spyOn(storage, 'setItem');
		const store = createWatchListStore({
			provider: fakeProvider().provider,
			storage: () => storage,
			owner: () => null
		});
		await store.load();
		store.apply({ kind: 'add', ids: [1] });
		expect(store.initial).toEqual([]);
		expect(setItem).not.toHaveBeenCalled();
	});
});
