/**
 * `monitorValues.ts` に対するユニットテスト。同じスタイル（describe/it、
 * 依存ゼロの純関数を直接 import）。
 *
 * 値の畳み込み（`mergeTagValues`・`replaceOrMergeTagValues`）を固定する。行への適用
 * （`applyTagValues`）は、タグモニタ（`/monitor`）を `/tags` に吸収したとき（#1255 段階 ③）に
 * 消した。
 */
import { describe, expect, it } from 'vitest';
import {
	keepAcceptedValues,
	mergeTagValues,
	replaceOrMergeTagValues,
	type RowValue
} from './monitorValues';

describe('mergeTagValues', () => {
	it('空マップへ values を畳み込むと外部名をキーにしたマップになる', () => {
		const merged = mergeTagValues(new Map(), [
			{ tag: 'plc1.groupA.D1', v: 1.5, q: 'good', t: 100 },
			{ tag: 'plc1.groupA.D2', v: 2.5, q: 'good', t: 100 }
		]);
		expect(merged.get('plc1.groupA.D1')).toEqual({ v: 1.5, q: 'good', t: 100 });
		expect(merged.get('plc1.groupA.D2')).toEqual({ v: 2.5, q: 'good', t: 100 });
		expect(merged.size).toBe(2);
	});

	it('既存マップを書き換えずに新しいマップを返す（純関数）', () => {
		const current = new Map([['plc1.groupA.D1', { v: 0, q: 'good', t: 1 }]]);
		const merged = mergeTagValues(current, [{ tag: 'plc1.groupA.D1', v: 9, q: 'good', t: 2 }]);
		expect(current.get('plc1.groupA.D1')).toEqual({ v: 0, q: 'good', t: 1 });
		expect(merged.get('plc1.groupA.D1')).toEqual({ v: 9, q: 'good', t: 2 });
		expect(merged).not.toBe(current);
	});

	it('同じタグが更新されると後の値で上書きする', () => {
		const merged = mergeTagValues(new Map(), [
			{ tag: 'plc1.groupA.D1', v: 1, q: 'good', t: 1 },
			{ tag: 'plc1.groupA.D1', v: 2, q: 'good', t: 2 }
		]);
		expect(merged.get('plc1.groupA.D1')).toEqual({ v: 2, q: 'good', t: 2 });
	});

	it('既存マップに無いタグは維持したまま追加する', () => {
		const current = new Map([['plc1.groupA.D1', { v: 0, q: 'good', t: 1 }]]);
		const merged = mergeTagValues(current, [{ tag: 'plc1.groupA.D2', v: 5, q: 'good', t: 5 }]);
		expect(merged.get('plc1.groupA.D1')).toEqual({ v: 0, q: 'good', t: 1 });
		expect(merged.get('plc1.groupA.D2')).toEqual({ v: 5, q: 'good', t: 5 });
	});
});

describe('mergeTagValues の unverified', () => {
	it('unverified は true のときだけ畳まれる', () => {
		const merged = mergeTagValues(new Map(), [
			{ tag: 'plc1.groupA.D1', v: null, q: 'bad', t: 1, unverified: true },
			{ tag: 'plc1.groupA.D2', v: 1, q: 'good', t: 1 }
		]);
		expect(merged.get('plc1.groupA.D1')).toEqual({ v: null, q: 'bad', t: 1, unverified: true });
		expect(merged.get('plc1.groupA.D2')).toEqual({ v: 1, q: 'good', t: 1 });
	});
});

describe('replaceOrMergeTagValues', () => {
	const cur = new Map<string, RowValue>([['old', { v: 1, q: 'good', t: 1 }]]);
	it('スナップショットは置き換える（空なら空）', () => {
		expect(replaceOrMergeTagValues(cur, [], true).size).toBe(0);
		const next = replaceOrMergeTagValues(cur, [{ tag: 'new', v: 2, q: 'good', t: 2 }], true);
		expect([...next.keys()]).toEqual(['new']);
		expect(cur.size).toBe(1);
	});
	it('差分はマージする', () => {
		const next = replaceOrMergeTagValues(cur, [{ tag: 'new', v: 2, q: 'good', t: 2 }], false);
		expect([...next.keys()].sort()).toEqual(['new', 'old']);
	});
});

describe('keepAcceptedValues（値を見張る窓、#1254）', () => {
	const val = (tag: string, v: number) => ({ tag, v, q: 'good', t: v });

	it('accept の外部名だけを残し、accept が無ければそのまま', () => {
		const values = [val('a', 1), val('b', 2)];
		expect(keepAcceptedValues(values, new Set(['a']))).toEqual([val('a', 1)]);
		expect(keepAcceptedValues(values, undefined)).toBe(values);
	});

	it('見張り外の値はマップに残らず、購読し直しのスナップショットに無い外部名の値は落ちる', () => {
		const watched = new Set(['a', 'b']);
		let map = replaceOrMergeTagValues(
			new Map(),
			keepAcceptedValues([val('a', 1), val('b', 2), val('x', 9)], watched),
			true
		);
		expect([...map.keys()].sort()).toEqual(['a', 'b']);
		map = replaceOrMergeTagValues(map, keepAcceptedValues([val('x', 10)], watched), false);
		expect([...map.keys()].sort()).toEqual(['a', 'b']);
		// b は別の所で消された: 購読し直しのスナップショットに無い。
		map = replaceOrMergeTagValues(map, keepAcceptedValues([val('a', 3)], watched), true);
		expect([...map.keys()]).toEqual(['a']);
		expect(map.get('a')?.v).toBe(3);
	});
});
