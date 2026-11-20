import { describe, expect, it } from 'vitest';
import { bitBandLabelVisible, bitBandLayout, bitSpans, resolveBitBandHeight } from './bitBands';
import type { Bin, TrendPen } from './pens';

const pen = (over: Partial<TrendPen> = {}): TrendPen => ({
	id: 'p',
	label: 'P',
	colorSlot: 1,
	bit: true,
	bitStyle: 'band',
	...over
});

describe('resolveBitBandHeight', () => {
	it('既定 16、8〜32 に丸める', () => {
		expect(resolveBitBandHeight(undefined)).toBe(16);
		expect(resolveBitBandHeight(null)).toBe(16);
		expect(resolveBitBandHeight(NaN)).toBe(16);
		expect(resolveBitBandHeight(2)).toBe(8);
		expect(resolveBitBandHeight(99)).toBe(32);
		expect(resolveBitBandHeight(20)).toBe(20);
	});
});

describe('bitBandLayout', () => {
	it('帯なし（step・bit でない）は描画域のまま', () => {
		const r = bitBandLayout(
			[pen({ bitStyle: 'step' }), pen({ bit: false }), pen({ bitStyle: null })],
			200
		);
		expect(r).toEqual({ bands: [], analogHeight: 200 });
	});
	it('1 本: 下から 16px', () => {
		const r = bitBandLayout([pen()], 200);
		expect(r.bands).toEqual([{ penIndex: 0, y: 184, height: 16, showLabel: true }]);
		expect(r.analogHeight).toBe(184);
	});
	it('複数: 並びの順に下から積む。帯でないペンは添字だけ飛ばす', () => {
		const r = bitBandLayout([pen(), pen({ bit: false }), pen({ bitBandHeight: 10 })], 200);
		expect(r.bands).toEqual([
			{ penIndex: 0, y: 184, height: 16, showLabel: true },
			{ penIndex: 2, y: 172, height: 10, showLabel: true }
		]);
		expect(r.analogHeight).toBe(200 - (16 + 10 + 2));
	});
	it('半分を超えたら同じ割合で縮めて半分に収める', () => {
		const r = bitBandLayout([pen({ bitBandHeight: 32 }), pen({ bitBandHeight: 32 })], 100);
		// raw = 66、上限 50
		const k = 50 / 66;
		expect(r.bands[0].height).toBeCloseTo(32 * k, 6);
		expect(r.bands[1].height).toBeCloseTo(32 * k, 6);
		expect(r.bands[0].y).toBeCloseTo(100 - 32 * k, 6);
		expect(r.analogHeight).toBeCloseTo(50, 6);
		expect(r.bands[1].y).toBeGreaterThanOrEqual(0);
	});
	it('縮んで細い帯は名前を出さない（高さ 160 に既定の 16 本）', () => {
		const r = bitBandLayout(
			Array.from({ length: 16 }, () => pen()),
			160
		);
		expect(r.bands).toHaveLength(16);
		expect(r.bands[0].height).toBeLessThan(10);
		expect(r.bands.every((b) => !b.showLabel)).toBe(true);
	});
	it('描画域が 0 以下・非有限なら帯なし', () => {
		expect(bitBandLayout([pen()], 0)).toEqual({ bands: [], analogHeight: 0 });
		expect(bitBandLayout([pen()], NaN)).toEqual({ bands: [], analogHeight: 0 });
	});
});

// 範囲 0〜100 を幅 100px → x = t
const X = (t: number) => t;
const R = { startMs: 0, endMs: 100 };
const b = (tMs: number, min: number | null, max: number | null = min): Bin => ({ tMs, min, max });

describe('bitSpans', () => {
	it('True は濃い塗り、False と欠測は塗らない', () => {
		const bins = [b(0, 1), b(10, 0), b(20, null), b(30, 1)];
		expect(bitSpans(bins, 10, X, R)).toEqual([
			{ x: 0, width: 10, state: 'true' },
			{ x: 30, width: 10, state: 'true' }
		]);
	});
	it('区間の中で変わった（0〜1）は mixed', () => {
		expect(bitSpans([b(0, 0, 1)], 10, X, R)).toEqual([{ x: 0, width: 10, state: 'mixed' }]);
	});
	it('隣り合う同じ状態は 1 つにまとめ、状態が違えば分ける', () => {
		const bins = [b(0, 1), b(10, 1), b(20, 0, 1), b(30, 0, 1), b(40, 1)];
		expect(bitSpans(bins, 10, X, R)).toEqual([
			{ x: 0, width: 20, state: 'true' },
			{ x: 20, width: 20, state: 'mixed' },
			{ x: 40, width: 10, state: 'true' }
		]);
	});
	it('間が空いていればまとめない', () => {
		expect(bitSpans([b(0, 1), b(20, 1)], 10, X, R)).toEqual([
			{ x: 0, width: 10, state: 'true' },
			{ x: 20, width: 10, state: 'true' }
		]);
	});
	it('範囲外の区間は飛ばし、端の区間は表示範囲で切る', () => {
		const bins = [b(-30, 1), b(-5, 1), b(95, 1), b(120, 1)];
		expect(bitSpans(bins, 10, X, R)).toEqual([
			{ x: 0, width: 5, state: 'true' },
			{ x: 95, width: 5, state: 'true' }
		]);
	});
	it('次の点が binMs より近ければそこで打ち切る', () => {
		expect(bitSpans([b(0, 1), b(4, 0)], 10, X, R)).toEqual([{ x: 0, width: 4, state: 'true' }]);
	});
	it('片方だけ有限な区間はその値の区間として扱う', () => {
		expect(bitSpans([b(0, null, 1)], 10, X, R)).toEqual([{ x: 0, width: 10, state: 'true' }]);
	});
	it('bins が空・binMs 不正なら空', () => {
		expect(bitSpans([], 10, X, R)).toEqual([]);
		expect(bitSpans([b(0, 1)], 0, X, R)).toEqual([]);
	});
});

describe('bitBandLabelVisible', () => {
	it('10px 以上で出し、未満・非有限では出さない', () => {
		expect(bitBandLabelVisible(10)).toBe(true);
		expect(bitBandLabelVisible(9.99)).toBe(false);
		expect(bitBandLabelVisible(16)).toBe(true);
		expect(bitBandLabelVisible(NaN)).toBe(false);
	});
});
