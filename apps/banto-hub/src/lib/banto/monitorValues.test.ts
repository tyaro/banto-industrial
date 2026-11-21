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
	keyValuesById,
	mergeTagValues,
	replaceOrMergeTagValues,
	tagIdValueKey,
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

describe('keyValuesById（値を見張る窓、#1254）', () => {
	const val = (tag: string, id: number | undefined, v: number) => ({ tag, id, v, q: 'good', t: v });

	it('ID のキーに付け替え、id の無い値は捨てる', () => {
		expect(keyValuesById([val('a', 1, 1), val('b', undefined, 2)])).toEqual([
			val(tagIdValueKey(1), 1, 1)
		]);
	});

	it('同じ外部名で作り直された別 ID の値は、旧 ID のキーに入らない', () => {
		let map = replaceOrMergeTagValues(new Map(), keyValuesById([val('c.g.a', 1, 5)]), true);
		expect(map.get(tagIdValueKey(1))?.v).toBe(5);
		// ID 1 を消し、同じ外部名で ID 2 を作り直した（購読し直しのスナップショットには 2 は来ないが、
		// 来たとしても）旧 ID の行には出ない。
		map = replaceOrMergeTagValues(map, keyValuesById([val('c.g.a', 2, 9)]), true);
		expect(map.get(tagIdValueKey(1))).toBeUndefined();
		expect(map.get(tagIdValueKey(2))?.v).toBe(9);
	});
});
