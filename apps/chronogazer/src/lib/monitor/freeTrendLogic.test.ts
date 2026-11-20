import { describe, expect, it } from 'vitest';
import {
	afterFreeTrendDelete,
	afterFreeTrendDeleteFailure,
	LAST_FREE_TREND_STORAGE_KEY,
	loadLastFreeTrend,
	parseFreeTrendParam,
	saveLastFreeTrend,
	selectFreeTrend,
	windowChoiceAfterSelection
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

describe('afterFreeTrendDelete', () => {
	const session = {
		startedOwner: 'account:alice' as string | null,
		currentOwner: 'account:alice' as string | null,
		startedSessionGen: 2,
		currentSessionGen: 2
	};
	const base = {
		...session,
		destroyed: false,
		currentRequestedId: 3 as number | null,
		deletedId: 3,
		navigating: false
	};
	it.each([
		['今の ?id が削除対象', {}, [true, true, true, true, true]],
		['別の ID を選んでいる', { currentRequestedId: 7 }, [true, true, true, false, true]],
		['?id が無い', { currentRequestedId: null }, [true, true, true, false, true]],
		['別の移動の最中', { navigating: true }, [true, true, true, false, true]],
		['同じセッションで破棄済み', { destroyed: true }, [true, true, false, false, false]],
		['セッションの世代が進んだ', { currentSessionGen: 3 }, [false, false, false, false, false]],
		['持ち主が変わった', { currentOwner: 'account:bob' }, [false, false, false, false, false]],
		[
			'セッションが替わり破棄済み',
			{ currentSessionGen: 3, destroyed: true },
			[false, false, false, false, false]
		]
	])('%s', (_name, patch, [clearDraft, notify, applyToPage, clearUrl, reload]) => {
		expect(afterFreeTrendDelete({ ...base, ...patch })).toEqual({
			clearDraft,
			notify,
			applyToPage,
			clearUrl,
			reload
		});
	});
});

describe('afterFreeTrendDeleteFailure', () => {
	const session = {
		startedOwner: 'account:alice' as string | null,
		currentOwner: 'account:alice' as string | null,
		startedSessionGen: 2,
		currentSessionGen: 2
	};
	it.each([
		['同じセッション', {}, { notify: true, reload: true }],
		['同じセッションで破棄済み', { destroyed: true }, { notify: true, reload: false }],
		['セッションの世代が進んだ', { currentSessionGen: 3 }, { notify: false, reload: false }],
		['持ち主がなくなった', { currentOwner: null }, { notify: false, reload: false }]
	])('%s', (_name, patch, expected) => {
		expect(afterFreeTrendDeleteFailure({ ...session, destroyed: false, ...patch })).toEqual(
			expected
		);
	});
});

describe('windowChoiceAfterSelection', () => {
	it('同じ自由トレンドなら残す', () => {
		const choice = { id: 3, sec: 60 };
		expect(windowChoiceAfterSelection(choice, 3)).toBe(choice);
	});
	it('A → B で捨てる（A に戻っても復活しない）', () => {
		let choice = windowChoiceAfterSelection({ id: 3, sec: 60 }, 7);
		expect(choice).toBeNull();
		choice = windowChoiceAfterSelection(choice, 3);
		expect(choice).toBeNull();
	});
	it('選択が無くなったら捨てる・一時値が無ければ無いまま', () => {
		expect(windowChoiceAfterSelection({ id: 3, sec: 60 }, null)).toBeNull();
		expect(windowChoiceAfterSelection(null, 3)).toBeNull();
	});
});
