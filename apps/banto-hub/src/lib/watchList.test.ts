import { describe, expect, it, vi } from 'vitest';
import {
	WATCH_LIMIT,
	addToWatch,
	applyWatchOps,
	parseWatchList,
	checkIdsConcurrently,
	withoutIds,
	removeFromWatch,
	sameWatchList,
	serializeWatchList,
	watchListKeys
} from './watchList';

describe('addToWatch', () => {
	it('後ろへ足し、入力の順を保つ', () => {
		expect(addToWatch([1], [3, 2])).toEqual({
			list: [1, 3, 2],
			added: 2,
			duplicates: 0,
			overLimit: 0
		});
	});

	it('重複（一覧にある・入力の中の重複）は足さない', () => {
		const result = addToWatch([1, 2], [2, 3, 3]);
		expect(result.list).toEqual([1, 2, 3]);
		expect(result.added).toBe(1);
		expect(result.duplicates).toBe(2);
	});

	it('上限を超える分は足さず、件数を返す', () => {
		const full = Array.from({ length: WATCH_LIMIT - 1 }, (_, i) => i);
		const result = addToWatch(full, [1000, 1001, 1002]);
		expect(result.list).toHaveLength(WATCH_LIMIT);
		expect(result.list.at(-1)).toBe(1000);
		expect(result.added).toBe(1);
		expect(result.overLimit).toBe(2);
	});

	it('元の配列は書き換えない', () => {
		const list = [1];
		addToWatch(list, [2]);
		expect(list).toEqual([1]);
	});
});

describe('removeFromWatch / withoutIds / sameWatchList', () => {
	it('1 件外す', () => {
		expect(removeFromWatch([1, 2, 3], 2)).toEqual([1, 3]);
	});

	it('与えた id を落とし、順は変えない', () => {
		expect(withoutIds([5, 1, 9, 3], new Set([9]))).toEqual([5, 1, 3]);
		expect(withoutIds([5, 1], new Set())).toEqual([5, 1]);
	});

	it('順序も含めて比べる', () => {
		expect(sameWatchList([1, 2], [1, 2])).toBe(true);
		expect(sameWatchList([1, 2], [2, 1])).toBe(false);
		expect(sameWatchList([1], [1, 2])).toBe(false);
	});
});

describe('parseWatchList / serializeWatchList', () => {
	it('往復できる', () => {
		expect(parseWatchList(serializeWatchList([3, 1, 2]))).toEqual([3, 1, 2]);
		expect(parseWatchList(serializeWatchList([]))).toEqual([]);
	});

	it('未保存・壊れた値・知らない形は undefined', () => {
		for (const raw of [
			null,
			undefined,
			'',
			'{broken',
			'[1]',
			'{"schema":2,"tags":[1]}',
			'{"schema":1}'
		]) {
			expect(parseWatchList(raw)).toBeUndefined();
		}
	});

	it('整数でない要素・重複を落とし、上限で切る', () => {
		expect(parseWatchList('{"schema":1,"tags":[1,"2",1.5,null,1,3]}')).toEqual([1, 3]);
		const many = JSON.stringify({ schema: 1, tags: Array.from({ length: 150 }, (_, i) => i) });
		expect(parseWatchList(many)).toHaveLength(WATCH_LIMIT);
	});
});

describe('watchListKeys', () => {
	it('写しは持ち主ごと、持ち主が無ければ写しなし', () => {
		expect(watchListKeys('account:1.x')).toEqual({
			local: 'banto.watch.v1.account%3A1%2Ex',
			setting: 'hub.watch.v1'
		});
		expect(watchListKeys(null).local).toBeUndefined();
		expect(watchListKeys('').local).toBeUndefined();
	});
});

describe('applyWatchOps', () => {
	it('足す・外す・すべて外すを順に当て、基準は書き換えない', () => {
		const base = [1, 2];
		expect(
			applyWatchOps(base, [
				{ kind: 'add', ids: [3, 2] },
				{ kind: 'remove', id: 1 },
				{ kind: 'add', ids: [4] }
			])
		).toEqual([2, 3, 4]);
		expect(applyWatchOps(base, [{ kind: 'clear' }, { kind: 'add', ids: [9] }])).toEqual([9]);
		expect(base).toEqual([1, 2]);
	});

	it('足すときの上限は addToWatch と同じ', () => {
		expect(applyWatchOps([1, 2], [{ kind: 'add', ids: [3, 4] }], 3)).toEqual([1, 2, 3]);
	});
});

describe('checkIdsConcurrently', () => {
	it('同時に走るのは concurrency 本まで。reject した id は結果に入れない', async () => {
		let running = 0;
		let peak = 0;
		const result = await checkIdsConcurrently(
			[1, 2, 3, 4, 5, 6, 7],
			async (id) => {
				running++;
				peak = Math.max(peak, running);
				await new Promise((resolve) => setTimeout(resolve, 0));
				running--;
				if (id === 3) throw new Error('500');
				return id % 2 === 0 ? 'gone' : 'exists';
			},
			2
		);
		expect(peak).toBe(2);
		expect([...result.entries()].sort((a, b) => a[0] - b[0])).toEqual([
			[1, 'exists'],
			[2, 'gone'],
			[4, 'gone'],
			[5, 'exists'],
			[6, 'gone'],
			[7, 'exists']
		]);
	});

	it('空の id では照会しない', async () => {
		const checkOne = vi.fn(async () => 'exists');
		expect((await checkIdsConcurrently([], checkOne)).size).toBe(0);
		expect(checkOne).not.toHaveBeenCalled();
	});
});
