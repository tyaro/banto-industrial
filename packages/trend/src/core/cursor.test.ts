import { describe, expect, it } from 'vitest';
import { binAt, cursorReadout } from './cursor';
import type { Bin, TrendPen } from './pens';

const bins: Bin[] = [
	{ tMs: 0, min: 1, max: 2 },
	{ tMs: 10, min: null, max: null },
	{ tMs: 20, min: 3.14159, max: 3.14159 },
	{ tMs: 40, min: 5, max: 6 }
];

describe('binAt', () => {
	it('tMs を含む区間を返す', () => {
		expect(binAt(bins, 10, 0)).toBe(bins[0]);
		expect(binAt(bins, 10, 9.9)).toBe(bins[0]);
		expect(binAt(bins, 10, 25)).toBe(bins[2]);
		expect(binAt(bins, 10, 49)).toBe(bins[3]);
	});
	it('範囲外・区間の隙間・欠測は null', () => {
		expect(binAt(bins, 10, -1)).toBeNull();
		expect(binAt(bins, 10, 50)).toBeNull();
		expect(binAt(bins, 10, 35)).toBeNull();
		expect(binAt(bins, 10, 15)).toBeNull();
		expect(binAt([], 10, 0)).toBeNull();
		expect(binAt(bins, 0, 0)).toBeNull();
		expect(binAt(bins, 10, NaN)).toBeNull();
	});
});

describe('cursorReadout', () => {
	const pens: TrendPen[] = [
		{ id: 'a', label: 'A', colorSlot: 1, unit: '℃', decimals: 1 },
		{ id: 'b', label: 'B', colorSlot: 2 },
		{ id: 'c', label: 'C', colorSlot: 3 }
	];
	it('ペンごとに値を書式化する。欠測・データ無しは「—」', () => {
		const out = cursorReadout(pens, { a: bins, b: bins }, 10, 5);
		expect(out).toEqual([
			{ penId: 'a', min: 1, max: 2, text: '1.0〜2.0 ℃' },
			{ penId: 'b', min: 1, max: 2, text: '1〜2' },
			{ penId: 'c', min: null, max: null, text: '—' }
		]);
	});
	it('値が 1 つなら 1 つだけ出す', () => {
		const out = cursorReadout(pens.slice(0, 1), { a: bins }, 10, 20);
		expect(out[0].text).toBe('3.1 ℃');
		expect(cursorReadout(pens.slice(0, 1), { a: bins }, 10, 15)[0].text).toBe('—');
	});
});
