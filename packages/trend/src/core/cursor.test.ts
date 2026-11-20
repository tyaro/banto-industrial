import { describe, expect, it } from 'vitest';
import { binAt, cursorDiff, cursorReadout, formatDuration, placeCursor } from './cursor';
import type { TrendSeries } from './data';
import type { Bin, TrendPen } from './pens';

const bins: Bin[] = [
	{ tMs: 0, min: 1, max: 2 },
	{ tMs: 10, min: null, max: null },
	{ tMs: 20, min: 3.14159, max: 3.14159 },
	{ tMs: 40, min: 5, max: 6 }
];

describe('binAt', () => {
	it('次の点より後の時刻では前の区間を返さない（周期が途中で変わる列）', () => {
		const b: Bin[] = [
			{ tMs: 0, min: 1, max: 2 },
			{ tMs: 4, min: null, max: null },
			{ tMs: 8, min: 3, max: 4 }
		];
		expect(binAt(b, 10, 3)).toBe(b[0]);
		expect(binAt(b, 10, 4)).toBeNull();
		expect(binAt(b, 10, 7)).toBeNull();
		expect(binAt(b, 10, 9)).toBe(b[2]);
	});
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

const S = (b: readonly Bin[], binMs: number): TrendSeries => ({ bins: b, binMs });
const ALL = { startMs: 0, endMs: 100 };

describe('cursorReadout', () => {
	const pens: TrendPen[] = [
		{ id: 'a', label: 'A', colorSlot: 1, unit: '℃', decimals: 1 },
		{ id: 'b', label: 'B', colorSlot: 2 },
		{ id: 'c', label: 'C', colorSlot: 3 }
	];
	it('ペンごとに値を書式化する。欠測・データ無しは「—」', () => {
		const out = cursorReadout(pens, { a: S(bins, 10), b: S(bins, 10) }, 5, ALL);
		expect(out).toEqual([
			{ penId: 'a', min: 1, max: 2, text: '1.0〜2.0 ℃' },
			{ penId: 'b', min: 1, max: 2, text: '1〜2' },
			{ penId: 'c', min: null, max: null, text: '—' }
		]);
	});
	it('値が 1 つなら 1 つだけ出す', () => {
		const out = cursorReadout(pens.slice(0, 1), { a: S(bins, 10) }, 20, ALL);
		expect(out[0].text).toBe('3.1 ℃');
		expect(cursorReadout(pens.slice(0, 1), { a: S(bins, 10) }, 15, ALL)[0].text).toBe('—');
	});
	it('有効範囲の外・有効範囲が null なら全ペン「—」', () => {
		const series = { a: S(bins, 10), b: S(bins, 10) };
		const eff = { startMs: 2, endMs: 8 };
		expect(cursorReadout(pens, series, 1, eff).map((c) => c.text)).toEqual(['—', '—', '—']);
		expect(cursorReadout(pens, series, 8, eff)[0].text).toBe('—');
		expect(cursorReadout(pens, series, 5, eff)[0].text).toBe('1.0〜2.0 ℃');
		expect(cursorReadout(pens, series, 5, null).map((c) => c.text)).toEqual(['—', '—', '—']);
	});
	it('ペンごとの binMs で区間を判定する', () => {
		const one: Bin[] = [{ tMs: 0, min: 1, max: 1 }];
		const out = cursorReadout(pens.slice(0, 2), { a: S(one, 1000), b: S(one, 5000) }, 3000, {
			startMs: 0,
			endMs: 10000
		});
		expect(out.map((c) => c.text)).toEqual(['—', '1']);
	});
	it('bit のペンは False・True・False〜True で出す（単位・桁は付けない）', () => {
		const bit: TrendPen = { id: 'x', label: 'X', colorSlot: 1, bit: true, unit: 'V', decimals: 2 };
		const bb: Bin[] = [
			{ tMs: 0, min: 0, max: 0 },
			{ tMs: 10, min: 1, max: 1 },
			{ tMs: 20, min: 0, max: 1 },
			{ tMs: 30, min: null, max: null }
		];
		const t = (ms: number) => cursorReadout([bit], { x: S(bb, 10) }, ms, ALL)[0].text;
		expect([t(5), t(15), t(25), t(35)]).toEqual(['False', 'True', 'False〜True', '—']);
	});
});

describe('placeCursor', () => {
	it('1 回目は A、2 回目は B', () => {
		const r1 = placeCursor({ a: null, b: null }, 100);
		expect(r1).toEqual({ state: { a: 100, b: null }, placed: 'a' });
		expect(placeCursor(r1.state, 200)).toEqual({ state: { a: 100, b: 200 }, placed: 'b' });
	});
	it('3 回目以降は近い方を移す', () => {
		expect(placeCursor({ a: 100, b: 200 }, 120)).toEqual({
			state: { a: 120, b: 200 },
			placed: 'a'
		});
		expect(placeCursor({ a: 100, b: 200 }, 190)).toEqual({
			state: { a: 100, b: 190 },
			placed: 'b'
		});
		expect(placeCursor({ a: 200, b: 100 }, 90).placed).toBe('b');
	});
	it('同じ距離なら A', () => {
		expect(placeCursor({ a: 100, b: 200 }, 150).placed).toBe('a');
	});
});

describe('cursorDiff', () => {
	const pen = (o: Partial<TrendPen> = {}): TrendPen =>
		({ id: 'x', label: 'x', colorSlot: 1, unit: '℃', decimals: 1, ...o }) as TrendPen;
	const v = (min: number | null, max: number | null = min) => ({
		penId: 'x',
		min,
		max,
		text: ''
	});
	it('1 値どうしは B − A、符号は常に付く', () => {
		expect(cursorDiff(pen(), v(10), v(12.34))).toBe('+2.3 ℃');
		expect(cursorDiff(pen(), v(12.34), v(10))).toBe('-2.3 ℃');
		expect(cursorDiff(pen(), v(5), v(5))).toBe('+0.0 ℃');
	});
	it('幅があれば中点どうしの差に「約」', () => {
		expect(cursorDiff(pen(), v(0, 2), v(4))).toBe('約 +3.0 ℃');
		expect(cursorDiff(pen(), v(4), v(0, 2))).toBe('約 -3.0 ℃');
	});
	it('桁と単位はペンに従う', () => {
		expect(cursorDiff(pen({ decimals: 0, unit: '' }), v(1), v(4.4))).toBe('+3');
	});
	it('欠測と bit は「—」', () => {
		expect(cursorDiff(pen(), v(null), v(1))).toBe('—');
		expect(cursorDiff(pen(), v(1), v(null))).toBe('—');
		expect(cursorDiff(pen({ bit: true }), v(0), v(1))).toBe('—');
	});
});

describe('formatDuration', () => {
	it('境界', () => {
		expect(formatDuration(0)).toBe('0秒');
		expect(formatDuration(999)).toBe('0秒');
		expect(formatDuration(12_000)).toBe('12秒');
		expect(formatDuration(59_000)).toBe('59秒');
		expect(formatDuration(300_000)).toBe('5分00秒');
		expect(formatDuration(3_600_000)).toBe('1時間00分00秒');
		expect(formatDuration(3_723_000)).toBe('1時間02分03秒');
		expect(formatDuration(86_400_000)).toBe('1日00時間00分');
		expect(formatDuration(2 * 86_400_000 + 3 * 3_600_000 + 4 * 60_000 + 59_000)).toBe(
			'2日03時間04分'
		);
	});
	it('負は符号を付ける', () => {
		expect(formatDuration(-5_000)).toBe('-5秒');
		expect(formatDuration(-3_723_000)).toBe('-1時間02分03秒');
		expect(formatDuration(-500)).toBe('0秒');
	});
});
