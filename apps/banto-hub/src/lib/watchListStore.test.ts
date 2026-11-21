import { describe, expect, it, vi } from 'vitest';
import type { UiSettingsProvider } from '@banto/admin-core';
import { MemoryStorage } from './banto/testing/hubHttp';

vi.mock('./banto/setup', () => ({ getUiSettings: () => undefined }));

import { createWatchListStore } from './watchListStore';
import { serializeWatchList } from './watchList';

function fakeProvider(initial?: string) {
	const values = new Map<string, string>();
	if (initial !== undefined) values.set('hub.watch.v1', initial);
	const set = vi.fn(async (key: string, value: string) => {
		values.set(key, value);
	});
	const provider = {
		get: vi.fn(async (key: string) => values.get(key) ?? null),
		set
	} as unknown as UiSettingsProvider;
	return { provider, values, set };
}

async function settle(): Promise<void> {
	for (let i = 0; i < 5; i++) await Promise.resolve();
}

describe('createWatchListStore', () => {
	it('最初の描画には写しを使う（壊れていれば空）', () => {
		const storage = new MemoryStorage();
		const server = fakeProvider();
		const deps = { provider: server.provider, storage: () => storage, owner: () => 'account:1' };
		storage.setItem('banto.watch.v1.account%3A1', serializeWatchList([4, 5]));
		expect(createWatchListStore(deps).initial).toEqual([4, 5]);
		storage.setItem('banto.watch.v1.account%3A1', '{broken');
		expect(createWatchListStore(deps).initial).toEqual([]);
	});

	it('最初の save は基準で、送らない。変更は写しとサーバーへ送る', async () => {
		const storage = new MemoryStorage();
		const server = fakeProvider();
		const store = createWatchListStore({
			provider: server.provider,
			storage: () => storage,
			owner: () => 'account:1'
		});
		store.save([]);
		await store.loadSaved();
		store.save([1, 2]);
		await settle();
		expect(server.set).toHaveBeenCalledTimes(1);
		expect(server.values.get('hub.watch.v1')).toBe(serializeWatchList([1, 2]));
		expect(storage.getItem('banto.watch.v1.account%3A1')).toBe(serializeWatchList([1, 2]));
	});

	it('サーバーの値を返し、送り返さない', async () => {
		const server = fakeProvider(serializeWatchList([7, 8]));
		const store = createWatchListStore({
			provider: server.provider,
			storage: () => new MemoryStorage(),
			owner: () => 'account:1'
		});
		store.save([]);
		expect(await store.loadSaved()).toEqual([7, 8]);
		store.save([7, 8]);
		await settle();
		expect(server.set).not.toHaveBeenCalled();
	});

	it('読み終わる前に利用者が変えたら、その変更を送りサーバーの値は返さない', async () => {
		const server = fakeProvider(serializeWatchList([7, 8]));
		const store = createWatchListStore({
			provider: server.provider,
			storage: () => new MemoryStorage(),
			owner: () => 'account:1'
		});
		store.save([]);
		const loading = store.loadSaved();
		store.save([9]);
		expect(await loading).toBeUndefined();
		await settle();
		expect(server.values.get('hub.watch.v1')).toBe(serializeWatchList([9]));
	});

	it('持ち主が分からなければ写しを読まず書かない', () => {
		const storage = new MemoryStorage();
		const setItem = vi.spyOn(storage, 'setItem');
		const store = createWatchListStore({
			provider: fakeProvider().provider,
			storage: () => storage,
			owner: () => null
		});
		store.save([]);
		store.save([1]);
		expect(store.initial).toEqual([]);
		expect(setItem).not.toHaveBeenCalled();
	});
});
