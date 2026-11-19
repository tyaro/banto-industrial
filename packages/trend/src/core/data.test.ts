import { describe, expect, it } from 'vitest';
import { cursorReadout } from './cursor';
import { drawableSeries, intersectRange, type TrendData } from './data';
import { envelopePath, hasFiniteValue } from './geometry';
import type { TrendPen } from './pens';

const pen: TrendPen = { id: 'a', label: 'A', colorSlot: 1 };
const id = (v: number) => v;
const xs = id as unknown as (v: number) => number;

// 取得 [0,10000)、最後の区間は tMs=9000・幅 3000（実データは [9000,10000)）。
const data: TrendData = {
	range: { startMs: 0, endMs: 10000 },
	series: { a: { bins: [{ tMs: 9000, min: 1, max: 2 }], binMs: 3000 } }
};

describe('intersectRange', () => {
	it('重なりを返し、重ならなければ null', () => {
		expect(intersectRange({ startMs: 0, endMs: 10 }, { startMs: 5, endMs: 20 })).toEqual({
			startMs: 5,
			endMs: 10
		});
		expect(intersectRange({ startMs: 0, endMs: 10 }, { startMs: 10, endMs: 20 })).toBeNull();
		expect(intersectRange({ startMs: 0, endMs: 10 }, { startMs: 12, endMs: 20 })).toBeNull();
	});
});

describe('drawableSeries', () => {
	it('data が null なら空', () => {
		expect(drawableSeries(null, { startMs: 0, endMs: 10 })).toEqual({
			effective: null,
			series: {}
		});
	});
	it('取得範囲の外へ移した表示範囲では線もカーソルも空（指摘の再現）', () => {
		const view = { startMs: 10000, endMs: 12000 };
		const d = drawableSeries(data, view);
		expect(d.effective).toBeNull();
		expect(d.series.a.bins).toEqual([]);
		expect(hasFiniteValue(d.series.a.bins)).toBe(false);
		expect(envelopePath(d.series.a.bins, 3000, xs, xs)).toBe('');
		expect(cursorReadout([pen], d.series, 11000, d.effective)[0].text).toBe('—');
	});
	it('取得範囲と重なる表示範囲では、x は有効範囲の区間の中央', () => {
		const view = { startMs: 9500, endMs: 12000 };
		const d = drawableSeries(data, view);
		expect(d.effective).toEqual({ startMs: 9500, endMs: 10000 });
		expect(d.series.a.bins).toHaveLength(1);
		const path = envelopePath(d.series.a.bins, 3000, xs, xs, d.effective!);
		expect(path).toContain('9750');
		expect(cursorReadout([pen], d.series, 9800, d.effective)[0].text).toBe('1〜2');
		expect(cursorReadout([pen], d.series, 11000, d.effective)[0].text).toBe('—');
	});
	it('ペンごとの binMs で絞る', () => {
		const two: TrendData = {
			range: { startMs: 0, endMs: 10000 },
			series: {
				a: { bins: [{ tMs: 0, min: 1, max: 1 }], binMs: 1000 },
				b: { bins: [{ tMs: 0, min: 2, max: 2 }], binMs: 5000 }
			}
		};
		const d = drawableSeries(two, { startMs: 2000, endMs: 4000 });
		expect(d.series.a.bins).toEqual([]);
		expect(d.series.b.bins).toHaveLength(1);
		expect(d.series.b.binMs).toBe(5000);
	});
});
