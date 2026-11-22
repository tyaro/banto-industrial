import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { UiSettingsProvider } from '@banto/admin-core';
import { MemoryStorage } from './banto/testing/hubHttp';

vi.mock('./banto/setup', () => ({ getUiSettings: () => undefined }));

import {
	RETRY_BASE_MS,
	RETRY_MAX_MS,
	createWatchListStore,
	type WatchListChange
} from './watchListStore';
import { WATCH_LIMIT, confirmedDeadIds, serializeWatchList } from './watchList';

const KEY = 'hub.watch.v1';
const LOCAL_KEY = 'banto.watch.v1.account%3A1';

/** サーバーの代わり。既定は即答。場面ごとに `get`・`set` の振る舞いを `mock...Once` で替える。 */
function fakeProvider(initial?: string) {
	const values = new Map<string, string>();
	if (initial !== undefined) values.set(KEY, initial);
	const get = vi.fn(async (key: string): Promise<string | null> => values.get(key) ?? null);
	const set = vi.fn(async (key: string, value: string): Promise<void> => {
		values.set(key, value);
	});
	const provider = { get, set } as unknown as UiSettingsProvider;
	/** 次の GET を `release()` まで返さない。 */
	function deferNextGet(): { release: () => void } {
		let release: () => void = () => undefined;
		get.mockImplementationOnce(
			(key: string) =>
				new Promise<string | null>((resolve) => {
					release = () => resolve(values.get(key) ?? null);
				})
		);
		return { release: () => release() };
	}
	return {
		provider,
		values,
		get,
		set,
		deferNextGet,
		saved: () => values.get(KEY)
	};
}

function authError(kind: 'unauthorized' | 'forbidden' = 'unauthorized'): Error {
	return Object.assign(new Error(kind), { body: { kind } });
}

async function settle(): Promise<void> {
	for (let i = 0; i < 20; i++) await Promise.resolve();
}

function setup(
	server: ReturnType<typeof fakeProvider>,
	extra: Partial<Parameters<typeof createWatchListStore>[0]> = {}
) {
	const storage = new MemoryStorage();
	const changes: WatchListChange[] = [];
	const store = createWatchListStore({
		provider: server.provider,
		storage: () => storage,
		owner: () => 'account:1',
		onChange: (change) => changes.push(change),
		...extra
	});
	return { store, storage, changes };
}

beforeEach(() => {
	vi.useFakeTimers();
});

afterEach(() => {
	vi.useRealTimers();
});

describe('createWatchListStore', () => {
	it('最初の描画には写しを使い、読めたらサーバーの値を一覧にして送り返さない', async () => {
		const server = fakeProvider(serializeWatchList([7, 8]));
		const storage = new MemoryStorage();
		storage.setItem(LOCAL_KEY, serializeWatchList([4, 5]));
		const store = createWatchListStore({
			provider: server.provider,
			storage: () => storage,
			owner: () => 'account:1'
		});
		expect(store.initial).toEqual([4, 5]);
		expect(store.list).toEqual([4, 5]);
		expect(store.status).toBe('loading');
		await store.load();
		expect(store.list).toEqual([7, 8]);
		expect(store.status).toBe('saved');
		expect(server.set).not.toHaveBeenCalled();
		expect(storage.getItem(LOCAL_KEY)).toBe(serializeWatchList([7, 8]));
		store.dispose();
	});

	it('1: GET が通り PUT が失敗したら、操作を残して retrying。再送で saved、次の読み込みでも残る', async () => {
		const server = fakeProvider(serializeWatchList([1]));
		const { store } = setup(server);
		await store.load();
		server.set.mockRejectedValueOnce(new Error('offline'));
		store.apply({ kind: 'add', ids: [2] });
		expect(store.status).toBe('saving');
		await settle();
		expect(store.status).toBe('retrying');
		expect(store.list).toEqual([1, 2]);
		expect(server.saved()).toBe(serializeWatchList([1]));
		await vi.advanceTimersByTimeAsync(RETRY_BASE_MS);
		expect(store.status).toBe('saved');
		expect(server.saved()).toBe(serializeWatchList([1, 2]));
		store.dispose();

		const next = setup(server);
		await next.store.load();
		expect(next.store.list).toEqual([1, 2]);
		next.store.dispose();
	});

	it('2: PUT は届いたが応答が失敗したら、次の周回は PUT なしで saved', async () => {
		const server = fakeProvider(serializeWatchList([1]));
		const { store } = setup(server);
		await store.load();
		server.set.mockImplementationOnce(async (key: string, value: string) => {
			server.values.set(key, value);
			throw new Error('response lost');
		});
		store.apply({ kind: 'add', ids: [2] });
		await settle();
		expect(store.status).toBe('retrying');
		await vi.advanceTimersByTimeAsync(RETRY_BASE_MS);
		expect(store.status).toBe('saved');
		expect(server.set).toHaveBeenCalledTimes(1);
		expect(store.list).toEqual([1, 2]);
		store.dispose();
	});

	it('3: 別のタブが足した分は消さない（サーバー [1,2]、表示 [1] で 3 を足すと [1,2,3]）', async () => {
		const server = fakeProvider(serializeWatchList([1]));
		const { store } = setup(server);
		await store.load();
		server.values.set(KEY, serializeWatchList([1, 2]));
		expect(store.list).toEqual([1]);
		store.apply({ kind: 'add', ids: [3] });
		await settle();
		expect(server.saved()).toBe(serializeWatchList([1, 2, 3]));
		expect(store.list).toEqual([1, 2, 3]);
		expect(store.status).toBe('saved');
		store.dispose();
	});

	it('4: タグ一覧 [1]・サーバー [1,2] なら 2 は消えた扱いにならず保存に残る', async () => {
		const server = fakeProvider(serializeWatchList([1, 2]));
		const { store } = setup(server);
		await store.load();
		store.setDeadIds(confirmedDeadIds(store.list, [1]));
		expect(store.list).toEqual([1, 2]);
		store.apply({ kind: 'add', ids: [5] });
		await settle();
		expect(server.saved()).toBe(serializeWatchList([1, 2, 5]));
		store.dispose();
	});

	it('5: タグ一覧 [1,3]・保存 [1,2,3] なら 2 は dead。操作なしでは PUT せず、次の足すで落ちる', async () => {
		const server = fakeProvider(serializeWatchList([1, 2, 3]));
		const { store } = setup(server);
		await store.load();
		store.setDeadIds(confirmedDeadIds(store.list, [1, 3]));
		expect(store.list).toEqual([1, 3]);
		// ページは除く前の id で判定し直す（除いた後の list で判定すると 2 が戻る）。
		expect([...confirmedDeadIds(store.deadCandidates, [1, 3])]).toEqual([2]);
		await settle();
		expect(server.set).not.toHaveBeenCalled();
		store.apply({ kind: 'add', ids: [4] });
		await settle();
		expect(server.saved()).toBe(serializeWatchList([1, 3, 4]));
		expect(store.list).toEqual([1, 3, 4]);
		store.dispose();
	});

	it('6: タグ一覧の最大より大きい消えた id は残る', async () => {
		const server = fakeProvider(serializeWatchList([1, 3, 9]));
		const { store } = setup(server);
		await store.load();
		store.setDeadIds(confirmedDeadIds(store.list, [1, 3]));
		expect(store.list).toEqual([1, 3, 9]);
		store.apply({ kind: 'remove', id: 1 });
		await settle();
		expect(server.saved()).toBe(serializeWatchList([3, 9]));
		store.dispose();
	});

	it('7: 周回中に来た操作は、その周回の成功で消えず、次の周回で送られる', async () => {
		const server = fakeProvider();
		const { store } = setup(server);
		await store.load();
		const gate = server.deferNextGet();
		store.apply({ kind: 'add', ids: [1] });
		await settle();
		store.apply({ kind: 'add', ids: [2] });
		expect(store.list).toEqual([1, 2]);
		gate.release();
		await settle();
		expect(server.set).toHaveBeenCalledTimes(2);
		expect(server.set.mock.calls[0][1]).toBe(serializeWatchList([1]));
		expect(server.set.mock.calls[1][1]).toBe(serializeWatchList([1, 2]));
		expect(store.status).toBe('saved');
		expect(store.list).toEqual([1, 2]);
		store.dispose();
	});

	it('8: GET の失敗が続けば待ちは倍々で上限まで、隠れている間は止まり、retryNow と新しい操作で打ち切る', async () => {
		const server = fakeProvider();
		server.get.mockRejectedValue(new Error('offline'));
		let shown = true;
		let wake: (() => void) | null = null;
		const { store } = setup(server, {
			visible: () => shown,
			onVisible: (listener) => {
				wake = listener;
				return () => {
					wake = null;
				};
			}
		});
		await store.load();
		expect(server.get).toHaveBeenCalledTimes(1);
		// 読めず操作も無い間は表示しない（裏で読み直す）。
		expect(store.status).toBe('loading');
		store.apply({ kind: 'add', ids: [1] });
		await settle();
		expect(server.get).toHaveBeenCalledTimes(2);
		expect(store.status).toBe('retrying');
		// 待ちは 2 回目の失敗で 4 秒（1 回目の 2 秒の待ちは新しい操作で打ち切った）。
		await vi.advanceTimersByTimeAsync(RETRY_BASE_MS * 2 - 1);
		expect(server.get).toHaveBeenCalledTimes(2);
		await vi.advanceTimersByTimeAsync(1);
		expect(server.get).toHaveBeenCalledTimes(3);
		await vi.advanceTimersByTimeAsync(RETRY_BASE_MS * 4);
		expect(server.get).toHaveBeenCalledTimes(4);
		store.retryNow();
		await settle();
		expect(server.get).toHaveBeenCalledTimes(5);
		store.apply({ kind: 'add', ids: [2] });
		await settle();
		expect(server.get).toHaveBeenCalledTimes(6);
		// 上限で止まる。
		await vi.advanceTimersByTimeAsync(RETRY_MAX_MS * 10);
		const calls = server.get.mock.calls.length;
		await vi.advanceTimersByTimeAsync(RETRY_MAX_MS - 1);
		expect(server.get.mock.calls.length).toBe(calls);
		await vi.advanceTimersByTimeAsync(1);
		expect(server.get.mock.calls.length).toBe(calls + 1);
		// 隠れている間は読まない。見えたらすぐ読む。
		shown = false;
		await vi.advanceTimersByTimeAsync(RETRY_MAX_MS * 3);
		expect(server.get.mock.calls.length).toBe(calls + 1);
		expect(wake).not.toBeNull();
		server.get.mockReset();
		server.get.mockImplementation(async () => server.values.get(KEY) ?? null);
		shown = true;
		wake!();
		await settle();
		expect(server.get).toHaveBeenCalledTimes(1);
		expect(store.status).toBe('saved');
		expect(server.saved()).toBe(serializeWatchList([1, 2]));
		store.dispose();
	});

	it('9a: unauthorized は blocked で、再送せず写しも書かない', async () => {
		const server = fakeProvider();
		const { store, storage } = setup(server);
		await store.load();
		storage.setItem(LOCAL_KEY, serializeWatchList([]));
		server.set.mockRejectedValueOnce(authError());
		store.apply({ kind: 'add', ids: [1] });
		await settle();
		expect(store.status).toBe('blocked');
		await vi.advanceTimersByTimeAsync(RETRY_MAX_MS * 3);
		store.retryNow();
		await settle();
		expect(server.set).toHaveBeenCalledTimes(1);
		expect(storage.getItem(LOCAL_KEY)).toBe(serializeWatchList([]));
		store.dispose();
	});

	it('9b: forbidden の読みも blocked', async () => {
		const server = fakeProvider();
		server.get.mockRejectedValueOnce(authError('forbidden'));
		const { store } = setup(server);
		await store.load();
		expect(store.status).toBe('blocked');
		await vi.advanceTimersByTimeAsync(RETRY_MAX_MS * 3);
		expect(server.get).toHaveBeenCalledTimes(1);
		store.dispose();
	});

	it('9c: 持ち主が替わったら blocked で、書かず写しも書かない', async () => {
		const server = fakeProvider(serializeWatchList([1]));
		let owner = 'account:1';
		const { store, storage } = setup(server, { owner: () => owner });
		await store.load();
		expect(storage.getItem(LOCAL_KEY)).toBe(serializeWatchList([1]));
		owner = 'account:2';
		store.apply({ kind: 'add', ids: [2] });
		await settle();
		expect(store.status).toBe('blocked');
		expect(server.set).not.toHaveBeenCalled();
		expect(storage.getItem(LOCAL_KEY)).toBe(serializeWatchList([1]));
		await vi.advanceTimersByTimeAsync(RETRY_MAX_MS * 3);
		expect(server.get).toHaveBeenCalledTimes(2);
		store.dispose();
	});

	it('10: dispose の後は周回の結果を当てず、知らせない', async () => {
		const server = fakeProvider(serializeWatchList([1]));
		const gate = server.deferNextGet();
		const { store, changes } = setup(server);
		void store.load();
		store.apply({ kind: 'add', ids: [2] });
		const seen = changes.length;
		store.dispose();
		gate.release();
		await settle();
		await vi.advanceTimersByTimeAsync(RETRY_MAX_MS);
		expect(changes.length).toBe(seen);
		expect(server.set).not.toHaveBeenCalled();
		expect(store.status).toBe('saving');
	});

	it('11: 壊れた保存値は、操作が無ければ書かず、操作を足すと上書きする', async () => {
		const server = fakeProvider('{broken');
		const { store } = setup(server);
		await store.load();
		expect(store.list).toEqual([]);
		expect(store.status).toBe('saved');
		expect(server.set).not.toHaveBeenCalled();
		store.apply({ kind: 'add', ids: [1] });
		await settle();
		expect(server.saved()).toBe(serializeWatchList([1]));
		store.dispose();
	});

	it('12: サーバーの値に当てて上限で落ちた件数を知らせる', async () => {
		const server = fakeProvider();
		const { store, changes } = setup(server);
		await store.load();
		const full = Array.from({ length: WATCH_LIMIT }, (_, i) => 1000 + i);
		server.values.set(KEY, serializeWatchList(full));
		const result = store.apply({ kind: 'add', ids: [1, 2] });
		expect(result.overLimit).toBe(0);
		await settle();
		expect(changes.some((c) => c.overLimit === 2)).toBe(true);
		expect(store.list).toEqual(full);
		expect(server.set).not.toHaveBeenCalled();
		store.dispose();
	});

	it('12b: 足した時点で知らせた上限超えは、もう一度は知らせない', async () => {
		const full = Array.from({ length: WATCH_LIMIT }, (_, i) => 1000 + i);
		const server = fakeProvider(serializeWatchList(full));
		const { store, changes } = setup(server);
		await store.load();
		const result = store.apply({ kind: 'add', ids: [1] });
		expect(result.overLimit).toBe(1);
		await settle();
		expect(changes.every((c) => c.overLimit === 0)).toBe(true);
		store.dispose();
	});

	it('13: 一度も読めていない間の表示は写しに操作を当てたもの。写しは成功の後だけ書く', async () => {
		const server = fakeProvider(serializeWatchList([7]));
		const gate = server.deferNextGet();
		const storage = new MemoryStorage();
		storage.setItem(LOCAL_KEY, serializeWatchList([4, 5]));
		const store = createWatchListStore({
			provider: server.provider,
			storage: () => storage,
			owner: () => 'account:1'
		});
		void store.load();
		store.apply({ kind: 'add', ids: [6] });
		expect(store.list).toEqual([4, 5, 6]);
		expect(store.status).toBe('saving');
		expect(storage.getItem(LOCAL_KEY)).toBe(serializeWatchList([4, 5]));
		gate.release();
		await settle();
		// 読むだけの周回（操作の数 0）の後に、足すの周回でサーバーの値に当てる。
		expect(store.list).toEqual([7, 6]);
		expect(server.saved()).toBe(serializeWatchList([7, 6]));
		expect(storage.getItem(LOCAL_KEY)).toBe(serializeWatchList([7, 6]));
		expect(store.status).toBe('saved');
		store.dispose();
	});

	it('持ち主が分からなければ写しを読まず書かない', async () => {
		const server = fakeProvider(serializeWatchList([1]));
		const storage = new MemoryStorage();
		const getItem = vi.spyOn(storage, 'getItem');
		const setItem = vi.spyOn(storage, 'setItem');
		const store = createWatchListStore({
			provider: server.provider,
			storage: () => storage,
			owner: () => null
		});
		await store.load();
		store.apply({ kind: 'add', ids: [2] });
		await settle();
		expect(server.saved()).toBe(serializeWatchList([1, 2]));
		expect(getItem).not.toHaveBeenCalled();
		expect(setItem).not.toHaveBeenCalled();
		store.dispose();
	});
});
