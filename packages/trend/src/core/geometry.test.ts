import { describe, expect, it } from 'vitest';
import { envelopePath, hasFiniteValue, visibleBins } from './geometry';
import { resolveYDomain } from './pens';
import type { TrendPen } from './pens';
import type { Bin } from './pens';

// x: 時刻をそのまま px に、y: 値を反転（上が大きい）。
const x = (t: number) => t;
const y = (v: number) => 100 - v;

describe('envelopePath', () => {
	it('各区間は区間の中央の縦線で、前の点に近い端から入る', () => {
		const bins: Bin[] = [
			{ tMs: 0, min: 10, max: 20 },
			{ tMs: 10, min: 15, max: 30 },
			{ tMs: 20, min: 0, max: 18 }
		];
		// 1 区間目: min→max（y 90→80）。2 区間目: 前の y=80 に近いのは max(30→y70)? |70-80|=10,
		// min(15→y85) は |85-80|=5 なので min から入る。3 区間目: 前の y=70、max(18→82) が近い。
		expect(envelopePath(bins, 10, x, y)).toBe('M5,90L5,80L15,85L15,70L25,82L25,100');
	});

	it('値が 1 つの区間は点 1 つ。片方だけ有限も 1 つ', () => {
		const bins: Bin[] = [
			{ tMs: 0, min: 10, max: 10 },
			{ tMs: 10, min: null, max: 20 },
			{ tMs: 20, min: 30, max: null }
		];
		expect(envelopePath(bins, 10, x, y)).toBe('M5,90L15,80L25,70');
	});

	it('欠測・非有限で線を切る。1 点だけの切れ端は同じ点への L を足す', () => {
		const bins: Bin[] = [
			{ tMs: 0, min: 10, max: 10 },
			{ tMs: 10, min: null, max: null },
			{ tMs: 20, min: 1, max: 2 },
			{ tMs: 30, min: NaN, max: Infinity },
			{ tMs: 40, min: 5, max: 5 }
		];
		expect(envelopePath(bins, 10, x, y)).toBe('M5,90L5,90M25,99L25,98M45,95L45,95');
	});

	it('空なら空文字。座標を小数 2 桁に丸める', () => {
		expect(envelopePath([], 10, x, y)).toBe('');
		expect(envelopePath([{ tMs: 0, min: 1 / 3, max: 1 / 3 }], 1 / 3, x, y)).toBe(
			'M0.17,99.67L0.17,99.67'
		);
	});

	it('min > max の区間も縦線にする', () => {
		expect(envelopePath([{ tMs: 0, min: 20, max: 10 }], 10, x, y)).toBe('M5,90L5,80');
	});

	it('区間の列に穴（binMs * 1.5 超）があれば線を切る。隣り合う区間はつながる', () => {
		const hole: Bin[] = [
			{ tMs: 0, min: 10, max: 10 },
			{ tMs: 3000, min: 20, max: 20 }
		];
		const p = envelopePath(hole, 1000, x, y);
		expect(p.match(/M/g)).toHaveLength(2);
		const near: Bin[] = [
			{ tMs: 0, min: 10, max: 10 },
			{ tMs: 1400, min: 20, max: 20 }
		];
		expect(envelopePath(near, 1000, x, y).match(/M/g)).toHaveLength(1);
	});

	it('表示範囲の端で欠ける区間の x は範囲の中に置く', () => {
		const bins: Bin[] = [{ tMs: 9000, min: 10, max: 10 }];
		expect(envelopePath(bins, 3000, x, y, { endMs: 10000 })).toBe('M9500,90L9500,90');
		expect(envelopePath(bins, 3000, x, y)).toBe('M10500,90L10500,90');
		const head: Bin[] = [{ tMs: -2000, min: 10, max: 10 }];
		expect(envelopePath(head, 3000, x, y, { startMs: 0 })).toBe('M500,90L500,90');
	});

	it('表示範囲と重ならない区間は描かない', () => {
		const stale: Bin[] = [
			{ tMs: 0, min: 1, max: 2 },
			{ tMs: 10, min: 1, max: 2 }
		];
		expect(envelopePath(stale, 10, (t) => t - 100, y, { startMs: 100, endMs: 200 })).toBe('');
		const bins: Bin[] = [
			{ tMs: 80, min: 10, max: 10 },
			{ tMs: 90, min: 10, max: 10 },
			{ tMs: 100, min: 20, max: 20 },
			{ tMs: 190, min: 30, max: 30 },
			{ tMs: 200, min: 40, max: 40 },
			{ tMs: 210, min: 50, max: 50 }
		];
		expect(envelopePath(bins, 10, x, y, { startMs: 100, endMs: 200 })).toBe(
			'M105,80L105,80M195,70L195,70'
		);
	});
});

describe('visibleBins / hasFiniteValue', () => {
	const range = { startMs: 100, endMs: 200 };
	const pen = { id: 'a', label: 'A' } as TrendPen;

	it('表示範囲の外にだけ値がある → 空で、縦軸は既定のまま', () => {
		const bins: Bin[] = [
			{ tMs: 0, min: 1000, max: 5000 },
			{ tMs: 300, min: 9000, max: 9999 }
		];
		const v = visibleBins(bins, 10, range);
		expect(v).toEqual([]);
		expect(hasFiniteValue(v)).toBe(false);
		expect(resolveYDomain(pen, v)).toEqual(resolveYDomain(pen, []));
		expect(resolveYDomain(pen, v)).toEqual([0, 1]);
	});

	it('一部だけ範囲内 → 重なる区間だけ残り、外の大きな値は縦軸に入らない', () => {
		const bins: Bin[] = [
			{ tMs: 50, min: 0, max: 99999 },
			{ tMs: 95, min: 1, max: 2 },
			{ tMs: 150, min: 3, max: 4 },
			{ tMs: 200, min: 0, max: 88888 }
		];
		const v = visibleBins(bins, 10, range);
		expect(v.map((b) => b.tMs)).toEqual([95, 150]);
		expect(hasFiniteValue(v)).toBe(true);
		const [, hi] = resolveYDomain(pen, v);
		expect(hi).toBeLessThan(100);
	});

	it('範囲の端をまたぐ区間は残る', () => {
		const bins: Bin[] = [
			{ tMs: 90, min: 1, max: 2 },
			{ tMs: 100, min: 1, max: 2 },
			{ tMs: 190, min: 1, max: 2 },
			{ tMs: 200, min: 1, max: 2 }
		];
		expect(visibleBins(bins, 20, range).map((b) => b.tMs)).toEqual([90, 100, 190]);
	});
});
