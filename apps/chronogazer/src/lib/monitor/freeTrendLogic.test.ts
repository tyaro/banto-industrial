import { describe, expect, it } from 'vitest';
import {
	LAST_FREE_TREND_STORAGE_KEY,
	loadLastFreeTrend,
	parseFreeTrendParam,
	saveLastFreeTrend,
	selectFreeTrend
} from './freeTrendLogic';

const list = [{ id: 3 }, { id: 7 }, { id: 9 }];

describe('parseFreeTrendParam', () => {
	it('数字だけを ID に読む', () => {
		expect(parseFreeTrendParam('7')).toBe(7);
		expect(parseFreeTrendParam(null)).toBeNull();
		expect(parseFreeTrendParam('')).toBeNull();
		expect(parseFreeTrendParam('abc')).toBeNull();
		expect(parseFreeTrendParam('-1')).toBeNull();
		expect(parseFreeTrendParam('1.5')).toBeNull();
	});
});

describe('selectFreeTrend', () => {
	it('?id が一覧にあればそれを選ぶ', () => {
		expect(selectFreeTrend(list, 7, 9)).toEqual({ id: 7, missingRequested: null });
	});
	it('?id が無ければ覚えたもの、それも無ければ先頭', () => {
		expect(selectFreeTrend(list, null, 9)).toEqual({ id: 9, missingRequested: null });
		expect(selectFreeTrend(list, null, 100)).toEqual({ id: 3, missingRequested: null });
		expect(selectFreeTrend(list, null, null)).toEqual({ id: 3, missingRequested: null });
	});
	it('?id が一覧に無ければ断る印を付け、別のものを選ぶ', () => {
		expect(selectFreeTrend(list, 42, 9)).toEqual({ id: 9, missingRequested: 42 });
		expect(selectFreeTrend(list, 42, null)).toEqual({ id: 3, missingRequested: 42 });
	});
	it('一覧が空なら選ばない', () => {
		expect(selectFreeTrend([], null, 1)).toEqual({ id: null, missingRequested: null });
		expect(selectFreeTrend([], 5, 1)).toEqual({ id: null, missingRequested: 5 });
	});
});

describe('端末の覚え', () => {
	it('保存して読み戻せる。壊れた値・使えないストレージは null', () => {
		const store = new Map<string, string>();
		const storage = {
			getItem: (k: string) => store.get(k) ?? null,
			setItem: (k: string, v: string) => void store.set(k, v)
		};
		saveLastFreeTrend(storage, 12);
		expect(store.get(LAST_FREE_TREND_STORAGE_KEY)).toBe('12');
		expect(loadLastFreeTrend(storage)).toBe(12);
		store.set(LAST_FREE_TREND_STORAGE_KEY, 'x');
		expect(loadLastFreeTrend(storage)).toBeNull();
		expect(loadLastFreeTrend(undefined)).toBeNull();
		expect(() => saveLastFreeTrend(undefined, 1)).not.toThrow();
		const throwing = {
			getItem: () => {
				throw new Error('denied');
			},
			setItem: () => {
				throw new Error('full');
			}
		};
		expect(loadLastFreeTrend(throwing)).toBeNull();
		expect(() => saveLastFreeTrend(throwing, 1)).not.toThrow();
	});
});
