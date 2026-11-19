import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { MemoryStorage } from './banto/testing/hubHttp';

// サーバー側の UiSettingsProvider の代役（キー → 値の Map）。
const store = new Map<string, string>();
const failing = { set: false, get: false };
const setLog: string[] = [];
let getOverride: (() => Promise<string | null>) | null = null;
vi.mock('./banto/setup', () => ({
	getUiSettings: () => ({
		get: async (key: string) => {
			if (failing.get) throw new Error('401');
			if (getOverride) {
				const override = getOverride;
				getOverride = null;
				return override();
			}
			return store.get(key) ?? null;
		},
		set: async (key: string, value: string) => {
			if (failing.set) throw new Error('401');
			setLog.push(key);
			store.set(key, value);
		}
	})
}));

import {
	loadRecentCommandIds,
	MAX_RECENT,
	parseRecentIds,
	RECENT_KEY,
	RECENT_SETTING,
	resetRecentSyncForTest,
	recordRecentCommand,
	syncRecentFromProvider
} from './recentCommands';

let storage: MemoryStorage;
beforeEach(() => {
	store.clear();
	failing.set = false;
	failing.get = false;
	getOverride = null;
	setLog.length = 0;
	resetRecentSyncForTest();
	storage = new MemoryStorage();
	vi.stubGlobal('localStorage', storage);
});
afterEach(() => {
	vi.unstubAllGlobals();
});

describe('parseRecentIds', () => {
	it('壊れた値・配列以外は空、文字列以外の要素は捨てる', () => {
		expect(parseRecentIds(null)).toEqual([]);
		expect(parseRecentIds('not json')).toEqual([]);
		expect(parseRecentIds('{"a":1}')).toEqual([]);
		expect(parseRecentIds('["a",1,"b"]')).toEqual(['a', 'b']);
	});
});

describe('recordRecentCommand（取り込みが済んだ後）', () => {
	beforeEach(async () => {
		await syncRecentFromProvider();
	});

	it('写しとサーバーの両方に、新しい順で書く（重複は先頭へ）', async () => {
		recordRecentCommand('a');
		recordRecentCommand('b');
		recordRecentCommand('a');
		await Promise.resolve();
		expect(loadRecentCommandIds()).toEqual(['a', 'b']);
		expect(JSON.parse(store.get(RECENT_SETTING) ?? 'null')).toEqual(['a', 'b']);
	});

	it(`${MAX_RECENT} 件を超えたら古いものから捨てる`, () => {
		for (let i = 0; i < MAX_RECENT + 3; i++) recordRecentCommand(`c${i}`);
		const ids = loadRecentCommandIds();
		expect(ids).toHaveLength(MAX_RECENT);
		expect(ids[0]).toBe(`c${MAX_RECENT + 2}`);
	});

	it('サーバーへの書き込みが失敗しても写しには入り、例外にならない', async () => {
		failing.set = true;
		recordRecentCommand('a');
		await Promise.resolve();
		expect(loadRecentCommandIds()).toEqual(['a']);
		expect(store.has(RECENT_SETTING)).toBe(false);
	});
});

/** 応答を手で返せる get（取り込み中の状態を作るため）。 */
function deferGet() {
	let resolve!: (value: string | null) => void;
	let reject!: (reason: unknown) => void;
	const promise = new Promise<string | null>((res, rej) => {
		resolve = res;
		reject = rej;
	});
	getOverride = () => promise;
	return { resolve, reject };
}

describe('syncRecentFromProvider', () => {
	it('サーバーの値が写しを上書きする', async () => {
		storage.setItem(RECENT_KEY, JSON.stringify(['local']));
		store.set(RECENT_SETTING, JSON.stringify(['server1', 'server2']));
		await syncRecentFromProvider();
		expect(loadRecentCommandIds()).toEqual(['server1', 'server2']);
	});

	it('始めた時点で写しを空にする（前の利用者の履歴を出さない）', async () => {
		storage.setItem(RECENT_KEY, JSON.stringify(['previous-user']));
		const d = deferGet();
		const p = syncRecentFromProvider();
		expect(loadRecentCommandIds()).toEqual([]);
		d.resolve(null);
		await p;
	});

	it('サーバーが null・壊れた値・空配列なら写しは空（前の利用者の履歴が残らない）', async () => {
		for (const raw of [null, 'broken', '[]']) {
			storage.setItem(RECENT_KEY, JSON.stringify(['previous-user']));
			if (raw === null) store.delete(RECENT_SETTING);
			else store.set(RECENT_SETTING, raw);
			await syncRecentFromProvider();
			expect(loadRecentCommandIds()).toEqual([]);
		}
		// 次の記録がサーバーへ書かれても、前の利用者の履歴は混ざらない。
		recordRecentCommand('mine');
		await Promise.resolve();
		expect(JSON.parse(store.get(RECENT_SETTING) ?? 'null')).toEqual(['mine']);
	});

	it('取り込み中の記録は写しにだけ書き、返ったらサーバー値とまとめて書き戻す', async () => {
		store.set(RECENT_SETTING, JSON.stringify(['s1', 'a', 's2']));
		const d = deferGet();
		const p = syncRecentFromProvider();
		recordRecentCommand('x');
		recordRecentCommand('a');
		await Promise.resolve();
		expect(loadRecentCommandIds()).toEqual(['a', 'x']);
		expect(JSON.parse(store.get(RECENT_SETTING) ?? 'null')).toEqual(['s1', 'a', 's2']); // まだ書かない
		d.resolve(store.get(RECENT_SETTING) ?? null);
		await p;
		const merged = ['a', 'x', 's1', 's2'];
		expect(loadRecentCommandIds()).toEqual(merged);
		expect(JSON.parse(store.get(RECENT_SETTING) ?? 'null')).toEqual(merged);
	});

	it('取り込み中の記録が無ければサーバーへ書き戻さない', async () => {
		store.set(RECENT_SETTING, JSON.stringify(['s1']));
		await syncRecentFromProvider();
		expect(setLog).toEqual([]);
	});

	it('取り込みが済む前（ログイン前）の記録は写しにだけ書く', async () => {
		recordRecentCommand('pre');
		await Promise.resolve();
		expect(store.has(RECENT_SETTING)).toBe(false);
	});

	it('読み取りが失敗したら写しの記録だけを持ち、サーバーへは書かない', async () => {
		storage.setItem(RECENT_KEY, JSON.stringify(['previous-user']));
		store.set(RECENT_SETTING, JSON.stringify(['server']));
		failing.get = true;
		await expect(syncRecentFromProvider()).resolves.toBeUndefined();
		expect(loadRecentCommandIds()).toEqual([]);
		recordRecentCommand('after');
		await Promise.resolve();
		expect(loadRecentCommandIds()).toEqual(['after']);
		expect(JSON.parse(store.get(RECENT_SETTING) ?? 'null')).toEqual(['server']);
		// 次のセッションの取り込みでやり直せる。
		failing.get = false;
		await syncRecentFromProvider();
		expect(loadRecentCommandIds()).toEqual(['server']);
	});

	it('古いセッションの応答は写しにもサーバーにも何もしない', async () => {
		store.set(RECENT_SETTING, JSON.stringify(['server']));
		storage.setItem(RECENT_KEY, JSON.stringify(['kept']));
		let current = true;
		const d = deferGet();
		const p = syncRecentFromProvider(() => current);
		recordRecentCommand('during');
		current = false;
		d.resolve(JSON.stringify(['stale']));
		await p;
		expect(loadRecentCommandIds()).toEqual(['during']); // 応答は当たっていない
		expect(JSON.parse(store.get(RECENT_SETTING) ?? 'null')).toEqual(['server']);
		// 始める前から古いなら、写しも空にしない。
		await syncRecentFromProvider(() => false);
		expect(loadRecentCommandIds()).toEqual(['during']);
	});
});
