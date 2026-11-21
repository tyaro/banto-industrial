import { afterEach, describe, expect, it, vi } from 'vitest';
import type { UiSettingsProvider } from '@banto/admin-core';
import { MemoryStorage } from './banto/testing/hubHttp';

vi.mock('./banto/setup', () => ({ getUiSettings: () => undefined }));

import { RETRY_BASE_MS, RETRY_MAX_MS, createWatchListStore } from './watchListStore';
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

/** 最初の `failures` 回の GET だけ失敗するサーバー（すぐ返る）。 */
function flakyProvider(initial: string | undefined, failures: number) {
	const values = new Map<string, string>();
	if (initial !== undefined) values.set('hub.watch.v1', initial);
	let left = failures;
	const set = vi.fn(async (key: string, value: string) => {
		values.set(key, value);
	});
	const get = vi.fn(async (key: string) => {
		if (left > 0) {
			left -= 1;
			throw new Error('offline');
		}
		return values.get(key) ?? null;
	});
	return { provider: { get, set } as unknown as UiSettingsProvider, values, get, set };
}

describe('createWatchListStore の読み直し（PR #1282 の再レビュー）', () => {
	afterEach(() => {
		vi.useRealTimers();
	});

	it('初回の読みが失敗したら待って読み直し、確定したら溜めた操作を保存する', async () => {
		vi.useFakeTimers();
		const server = flakyProvider(serializeWatchList([1]), 1);
		const onChange = vi.fn();
		const store = createWatchListStore({
			provider: server.provider,
			storage: () => new MemoryStorage(),
			owner: () => 'account:1',
			visible: () => true,
			onChange
		});
		// 読み込み中に足す（読み中なので前倒しはしない）。
		const first = store.load();
		store.apply({ kind: 'add', ids: [2] });
		expect(await first).toBe(false);
		expect(store.unsaved).toBe(true);
		expect(server.set).not.toHaveBeenCalled();
		expect(server.get).toHaveBeenCalledTimes(1);
		await vi.advanceTimersByTimeAsync(RETRY_BASE_MS - 1);
		expect(server.get).toHaveBeenCalledTimes(1);
		await vi.advanceTimersByTimeAsync(1);
		await settle();
		expect(server.get).toHaveBeenCalledTimes(2);
		expect(store.confirmed).toBe(true);
		expect(store.unsaved).toBe(false);
		expect(store.list).toEqual([1, 2]);
		expect(server.values.get('hub.watch.v1')).toBe(serializeWatchList([1, 2]));
		expect(onChange).toHaveBeenCalled();
		store.dispose();
	});

	it('確定する前の操作は、再試行の待ちを打ち切ってすぐ読み直す', async () => {
		vi.useFakeTimers();
		const server = flakyProvider(serializeWatchList([1]), 1);
		const store = createWatchListStore({
			provider: server.provider,
			storage: () => new MemoryStorage(),
			owner: () => 'account:1',
			visible: () => true
		});
		expect(await store.load()).toBe(false);
		expect(server.get).toHaveBeenCalledTimes(1);
		store.apply({ kind: 'add', ids: [3] });
		await settle();
		expect(server.get).toHaveBeenCalledTimes(2);
		expect(store.confirmed).toBe(true);
		expect(server.values.get('hub.watch.v1')).toBe(serializeWatchList([1, 3]));
		// 待っていた再試行は取り消され、もう読まない。
		await vi.advanceTimersByTimeAsync(RETRY_MAX_MS);
		expect(server.get).toHaveBeenCalledTimes(2);
		store.dispose();
	});

	it('待ちは倍々に延び、上限で止まる', async () => {
		vi.useFakeTimers();
		const server = flakyProvider(undefined, 100);
		const store = createWatchListStore({
			provider: server.provider,
			storage: () => new MemoryStorage(),
			owner: () => 'account:1',
			visible: () => true
		});
		await store.load();
		const calls = () => server.get.mock.calls.length;
		await vi.advanceTimersByTimeAsync(2_000);
		expect(calls()).toBe(2);
		await vi.advanceTimersByTimeAsync(3_999);
		expect(calls()).toBe(2);
		await vi.advanceTimersByTimeAsync(1);
		expect(calls()).toBe(3);
		// 8・16・32 秒、その後は 60 秒ごと。
		await vi.advanceTimersByTimeAsync(8_000 + 16_000 + 32_000);
		expect(calls()).toBe(6);
		await vi.advanceTimersByTimeAsync(RETRY_MAX_MS - 1);
		expect(calls()).toBe(6);
		await vi.advanceTimersByTimeAsync(1);
		expect(calls()).toBe(7);
		store.dispose();
	});

	it('dispose すると読み直しを止める', async () => {
		vi.useFakeTimers();
		const server = flakyProvider(undefined, 100);
		const store = createWatchListStore({
			provider: server.provider,
			storage: () => new MemoryStorage(),
			owner: () => 'account:1',
			visible: () => true
		});
		await store.load();
		store.dispose();
		await vi.advanceTimersByTimeAsync(RETRY_MAX_MS * 3);
		expect(server.get).toHaveBeenCalledTimes(1);
	});

	it('隠れている間は読まず、見えたらすぐ読む', async () => {
		vi.useFakeTimers();
		const server = flakyProvider(serializeWatchList([1]), 1);
		let shown = false;
		let wake: (() => void) | null = null;
		const store = createWatchListStore({
			provider: server.provider,
			storage: () => new MemoryStorage(),
			owner: () => 'account:1',
			visible: () => shown,
			onVisible: (listener) => {
				wake = listener;
				return () => {
					wake = null;
				};
			}
		});
		await store.load();
		await vi.advanceTimersByTimeAsync(RETRY_MAX_MS);
		expect(server.get).toHaveBeenCalledTimes(1);
		expect(wake).not.toBeNull();
		shown = true;
		wake!();
		await settle();
		expect(server.get).toHaveBeenCalledTimes(2);
		expect(store.confirmed).toBe(true);
		store.dispose();
	});
});
