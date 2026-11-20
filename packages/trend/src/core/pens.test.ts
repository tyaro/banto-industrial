import { niceTicks } from '@banto/charts';
import { describe, expect, it } from 'vitest';
import {
	axisOwners,
	axisTickValues,
	dataExtent,
	formatBitValue,
	formatValue,
	isBitBand,
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
	it('データの範囲で決める側だけ、管理線の値も範囲に含める', () => {
		const lim = (value: number) => ({
			value,
			side: 'upper' as const,
			colorVar: '--c',
			label: 'H'
		});
		// データ [-1, 5]。管理線 8 と -3 で両側が広がる。
		expect(resolveYDomain(pen({ limits: [lim(8), lim(-3)] }), bins)).toEqual([-3, 8]);
		// 範囲の内側の管理線は動かさない。
		expect(resolveYDomain(pen({ limits: [lim(2)] }), bins)).toEqual([-1, 5]);
		// ペンの指定・工学値レンジで決まる側は広げない。
		expect(resolveYDomain(pen({ yMax: 10, limits: [lim(20), lim(-3)] }), bins)).toEqual([-3, 10]);
		expect(
			resolveYDomain(pen({ engRange: { min: 0, max: 50 }, limits: [lim(80), lim(-9)] }), bins)
		).toEqual([0, 50]);
	});
	it('データが無く管理線だけでも、管理線の値で決める。非有限は無視する', () => {
		const lim = (value: number) => ({
			value,
			side: 'upper' as const,
			colorVar: '--c',
			label: 'H'
		});
		expect(resolveYDomain(pen({ limits: [lim(10), lim(20)] }), [])).toEqual([10, 20]);
		expect(resolveYDomain(pen({ limits: [lim(Number.NaN), lim(Infinity)] }), [])).toEqual([0, 1]);
		expect(resolveYDomain(pen({ limits: [lim(Number.NaN), lim(9)] }), bins)).toEqual([-1, 9]);
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

describe('帯の bit ペン', () => {
	it('isBitBand は bit かつ band のときだけ', () => {
		expect(isBitBand(pen({ bit: true, bitStyle: 'band' }))).toBe(true);
		expect(isBitBand(pen({ bit: true, bitStyle: 'step' }))).toBe(false);
		expect(isBitBand(pen({ bit: true }))).toBe(false);
		expect(isBitBand(pen({ bit: false, bitStyle: 'band' }))).toBe(false);
	});
	it('axisOwners は帯のペンを持ち主にしない', () => {
		const band = pen({ bit: true, bitStyle: 'band' });
		expect(axisOwners([band, pen()])).toEqual({ left: 1, right: null });
		expect(axisOwners([band, pen({ axis: 'right' })])).toEqual({ left: null, right: 1 });
		expect(axisOwners([band])).toEqual({ left: null, right: null });
		expect(axisOwners([pen({ bit: true, bitStyle: 'step' })])).toEqual({ left: 0, right: null });
	});
});

describe('formatBitValue', () => {
	it('0.5 以上が True、未満が False、跨ぐなら False〜True、欠測は —', () => {
		expect(formatBitValue(1, 1)).toBe('True');
		expect(formatBitValue(0, 0)).toBe('False');
		expect(formatBitValue(0, 1)).toBe('False〜True');
		expect(formatBitValue(0.5, 0.5)).toBe('True');
		expect(formatBitValue(0.4, 0.4)).toBe('False');
		expect(formatBitValue(null, 1)).toBe('True');
		expect(formatBitValue(null, null)).toBe('—');
		expect(formatBitValue(NaN, undefined)).toBe('—');
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

describe('axisTickValues', () => {
	it('bit のペンは 0 と 1 だけ', () => {
		expect(axisTickValues(pen({ bit: true }), [0, 1], 6)).toEqual([0, 1]);
		expect(axisTickValues(pen({ bit: true }), [-0.5, 1.5], 10)).toEqual([0, 1]);
	});
	it('bit でなければ niceTicks を範囲で絞ったもの', () => {
		expect(axisTickValues(pen(), [0, 100], 5)).toEqual(
			niceTicks(0, 100, 5).filter((v) => v >= 0 && v <= 100)
		);
	});
	it('bit で範囲が 0〜1 を含まなければ、含まないものは出さない', () => {
		expect(axisTickValues(pen({ bit: true }), [0.5, 2], 5)).toEqual([1]);
		expect(axisTickValues(pen({ bit: true }), [2, 5], 5)).toEqual([]);
	});
});
