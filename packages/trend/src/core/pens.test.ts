import { describe, expect, it } from 'vitest';
import {
	axisOwners,
	dataExtent,
	formatValue,
	penColorVar,
	resolveYDomain,
	type Bin,
	type TrendPen
} from './pens';

const pen = (over: Partial<TrendPen> = {}): TrendPen => ({
	id: 'p',
	label: 'P',
	colorSlot: 1,
	...over
});
const bins: Bin[] = [
	{ tMs: 0, min: 2, max: 5 },
	{ tMs: 1, min: null, max: null },
	{ tMs: 2, min: -1, max: 3 },
	{ tMs: 3, min: NaN, max: Infinity }
];

describe('dataExtent', () => {
	it('有限の値だけで範囲を取る', () => {
		expect(dataExtent(bins)).toEqual([-1, 5]);
		expect(dataExtent([{ tMs: 0, min: null, max: 7 }])).toEqual([7, 7]);
		expect(dataExtent([{ tMs: 0, min: null, max: null }])).toBeNull();
		expect(dataExtent([])).toBeNull();
	});
});

describe('resolveYDomain', () => {
	it('ペンの指定 → 工学値レンジ → データ → [0,1] の順', () => {
		expect(
			resolveYDomain(pen({ yMin: 0, yMax: 100, engRange: { min: 0, max: 50 } }), bins)
		).toEqual([0, 100]);
		expect(resolveYDomain(pen({ engRange: { min: 0, max: 50 } }), bins)).toEqual([0, 50]);
		expect(resolveYDomain(pen(), bins)).toEqual([-1, 5]);
		expect(resolveYDomain(pen(), [])).toEqual([0, 1]);
	});
	it('片側だけの指定は、もう片側を次の順位から取る', () => {
		expect(resolveYDomain(pen({ yMin: 0 }), bins)).toEqual([0, 5]);
		expect(resolveYDomain(pen({ yMax: 10 }), bins)).toEqual([-1, 10]);
		expect(resolveYDomain(pen({ yMax: 10, engRange: { min: -5, max: 5 } }), bins)).toEqual([
			-5, 10
		]);
	});
	it('min == max は上下に幅を持たせる', () => {
		const flat = [{ tMs: 0, min: 50, max: 50 }];
		expect(resolveYDomain(pen(), flat)).toEqual([45, 55]);
		expect(resolveYDomain(pen(), [{ tMs: 0, min: 0, max: 0 }])).toEqual([-1, 1]);
	});
	it('min == max で片側が指定なら、指定の側は動かさない', () => {
		expect(resolveYDomain(pen({ yMin: 0 }), [{ tMs: 0, min: 0, max: 0 }])).toEqual([0, 2]);
		expect(resolveYDomain(pen({ yMax: 50 }), [{ tMs: 0, min: 50, max: 50 }])).toEqual([40, 50]);
	});
	it('逆転したら、片側指定なら指定の外に置き直し、両側指定なら入れ替える', () => {
		expect(resolveYDomain(pen({ yMin: 100 }), bins)).toEqual([100, 120]);
		expect(resolveYDomain(pen({ yMax: -10 }), bins)).toEqual([-12, -10]);
		expect(resolveYDomain(pen({ yMin: 10, yMax: 0 }), bins)).toEqual([0, 10]);
	});
	it('非有限の指定・工学値レンジは無いものとして扱う', () => {
		expect(resolveYDomain(pen({ yMin: NaN, yMax: Infinity }), bins)).toEqual([-1, 5]);
		expect(resolveYDomain(pen({ engRange: { min: NaN, max: 1 } }), bins)).toEqual([-1, 5]);
	});
});

describe('axisOwners', () => {
	it('左右それぞれ先頭のペン', () => {
		expect(
			axisOwners([pen({ axis: 'right' }), pen(), pen({ axis: 'left' }), pen({ axis: 'right' })])
		).toEqual({ left: 1, right: 0 });
		expect(axisOwners([pen()])).toEqual({ left: 0, right: null });
		expect(axisOwners([])).toEqual({ left: null, right: null });
	});
});

describe('penColorVar', () => {
	it('1〜8 は banto、9〜16 は --bi-chart-N に banto の色の予備', () => {
		expect(penColorVar(1)).toBe('var(--banto-chart-1)');
		expect(penColorVar(8)).toBe('var(--banto-chart-8)');
		expect(penColorVar(9)).toBe('var(--bi-chart-9, var(--banto-chart-1))');
		expect(penColorVar(16)).toBe('var(--bi-chart-16, var(--banto-chart-8))');
	});
	it('範囲外は丸め、非有限は 1', () => {
		expect(penColorVar(0)).toBe('var(--banto-chart-1)');
		expect(penColorVar(99)).toBe('var(--bi-chart-16, var(--banto-chart-8))');
		expect(penColorVar(NaN)).toBe('var(--banto-chart-1)');
		expect(penColorVar(2.4)).toBe('var(--banto-chart-2)');
	});
});

describe('formatValue', () => {
	it('桁の指定があれば固定、無ければ有効数字 6 桁', () => {
		expect(formatValue(1.23456, 2)).toBe('1.23');
		expect(formatValue(3, 0)).toBe('3');
		expect(formatValue(1 / 3)).toBe('0.333333');
		expect(formatValue(120)).toBe('120');
		expect(formatValue(1.5, 1.5)).toBe('1.5');
		expect(formatValue(1.5, -1)).toBe('1.5');
	});
	it('null・非有限は「—」', () => {
		expect(formatValue(null)).toBe('—');
		expect(formatValue(undefined)).toBe('—');
		expect(formatValue(NaN, 2)).toBe('—');
	});
});
