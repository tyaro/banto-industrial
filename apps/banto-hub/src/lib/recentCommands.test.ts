import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { MemoryStorage } from './banto/testing/hubHttp';

// サーバー側の UiSettingsProvider の代役（キー → 値の Map）。
const store = new Map<string, string>();
const failing = { set: false, get: false };
vi.mock('./banto/setup', () => ({
	getUiSettings: () => ({
		get: async (key: string) => {
			if (failing.get) throw new Error('401');
			return store.get(key) ?? null;
		},
		set: async (key: string, value: string) => {
			if (failing.set) throw new Error('401');
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
	recordRecentCommand,
	syncRecentFromProvider
} from './recentCommands';

let storage: MemoryStorage;
beforeEach(() => {
	store.clear();
	failing.set = false;
	failing.get = false;
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

describe('recordRecentCommand', () => {
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

describe('syncRecentFromProvider', () => {
	it('サーバーの値が写しを上書きする', async () => {
		storage.setItem(RECENT_KEY, JSON.stringify(['local']));
		store.set(RECENT_SETTING, JSON.stringify(['server1', 'server2']));
		await syncRecentFromProvider();
		expect(loadRecentCommandIds()).toEqual(['server1', 'server2']);
	});

	it('サーバーに値が無い・壊れているときは写しのまま', async () => {
		storage.setItem(RECENT_KEY, JSON.stringify(['local']));
		await syncRecentFromProvider();
		expect(loadRecentCommandIds()).toEqual(['local']);
		store.set(RECENT_SETTING, 'broken');
		await syncRecentFromProvider();
		expect(loadRecentCommandIds()).toEqual(['local']);
	});

	it('読み取りが失敗しても写しのまま（例外にしない）', async () => {
		storage.setItem(RECENT_KEY, JSON.stringify(['local']));
		failing.get = true;
		await expect(syncRecentFromProvider()).resolves.toBeUndefined();
		expect(loadRecentCommandIds()).toEqual(['local']);
	});
});
