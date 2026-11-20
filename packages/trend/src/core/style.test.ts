import { describe, expect, it } from 'vitest';
import { envelopePath } from './geometry';
import type { Bin } from './pens';
import {
	dashArray,
	markerPath,
	markerPoints,
	resolveLineStyle,
	resolveLineWidth,
	resolveMarkerKind,
	resolveMarkerSize,
	swatchDashArray,
	swatchWidth
} from './style';

const x = (v: number) => v;
const y = (v: number) => 100 - v;
const bin = (tMs: number, min: number | null, max: number | null = min): Bin => ({ tMs, min, max });

describe('丸め', () => {
	it('既定と範囲', () => {
		expect(resolveLineWidth(null)).toBe(1.5);
		expect(resolveLineWidth(undefined)).toBe(1.5);
		expect(resolveLineWidth(NaN)).toBe(1.5);
		expect(resolveLineWidth(0)).toBe(1);
		expect(resolveLineWidth(9)).toBe(4);
		expect(resolveLineWidth(2.5)).toBe(2.5);
		expect(resolveMarkerSize(null)).toBe(4);
		expect(resolveMarkerSize(1)).toBe(2);
		expect(resolveMarkerSize(20)).toBe(8);
		expect(resolveMarkerSize(3.6)).toBe(4);
		expect(resolveLineStyle('x')).toBe('solid');
		expect(resolveLineStyle(null)).toBe('solid');
		expect(resolveMarkerKind('x')).toBe('none');
		expect(resolveMarkerKind('diamond')).toBe('diamond');
	});
});

describe('dashArray', () => {
	it('実線・線なしは null', () => {
		expect(dashArray('solid', 2)).toBeNull();
		expect(dashArray(null, 2)).toBeNull();
		expect(dashArray('none', 2)).toBeNull();
	});
	it('線幅に比例', () => {
		expect(dashArray('dashed', 2)).toBe('10 6');
		expect(dashArray('dashDot', 2)).toBe('10 5 0 5');
		expect(dashArray('dashDotDot', 1)).toBe('5 2.5 0 2.5 0 2.5');
		expect(dashArray('dashed', null)).toBe('7.5 4.5');
		expect(dashArray('dashed', 1.25)).toBe('6.25 3.75');
	});
});

describe('swatchDashArray', () => {
	const SWATCH_LINE = 30; // 見本の線分（幅 32 − 両端 1px）
	const len = (d: string, w: number) => d.split(' ').reduce((a, v) => a + Number(v), 0) + w; // 丸い端で伸びる分
	it('実線・線なしは null', () => {
		expect(swatchDashArray('solid', 2)).toBeNull();
		expect(swatchDashArray('none', 2)).toBeNull();
	});
	it('線幅 1〜4 の各線種で 1 巡が見本の線分に収まる', () => {
		for (const style of ['dashed', 'dashDot', 'dashDotDot']) {
			for (const lw of [1, 2, 3, 4]) {
				const d = swatchDashArray(style, lw) as string;
				expect(len(d, swatchWidth(lw))).toBeLessThanOrEqual(SWATCH_LINE);
			}
		}
	});
	it('一点鎖線と二点鎖線は違うパターン', () => {
		for (const lw of [1, 2, 3, 4]) {
			expect(swatchDashArray('dashDot', lw)).not.toBe(swatchDashArray('dashDotDot', lw));
		}
	});
});

describe('markerPoints', () => {
	const plot = { width: 1000, height: 100 };
	const sx = (v: number) => v / 100; // 100 秒 → 1000px ではなく 1 ms = 0.01px にするための換算
	const sy = (v: number) => 100 - v;
	it('描画域の外の点は間引きに数えず、すぐ後の見える点を残す', () => {
		// 区間 500 ms = 5px 幅。値 200（外）→ 50（内）。x は 2.5 と 7.5
		const bins = [bin(0, 200), bin(500, 50)];
		const p = markerPoints(bins, 500, sx, sy, undefined, 4, plot);
		expect(p).toEqual([{ x: 7.5, y: 50 }]);
	});
	it('範囲を渡さなければ今までと同じ（外の点が先に印を付ける）', () => {
		const bins = [bin(0, 200), bin(500, 50)];
		expect(markerPoints(bins, 500, sx, sy, undefined, 4)).toEqual([{ x: 2.5, y: -100 }]);
	});
	it('min と max の片方だけ見えるなら見える方を残す', () => {
		const bins = [bin(0, 50, 200)];
		const p = markerPoints(bins, 500, sx, sy, undefined, 4, plot);
		expect(p).toEqual([{ x: 2.5, y: 50 }]);
	});
	it('値が 1 つなら 1 点、離れた min/max は 2 点、近ければ中点 1 点', () => {
		const bins = [bin(0, 10), bin(100, 10, 30), bin(200, 10, 12)];
		const p = markerPoints(bins, 100, x, y, undefined, 4);
		expect(p).toEqual([
			{ x: 50, y: 90 },
			{ x: 150, y: 90 },
			{ x: 150, y: 70 },
			{ x: 250, y: 89 }
		]);
	});
	it('片方だけ有限は 1 点、欠測は飛ばす', () => {
		const bins = [bin(0, null, 5), bin(100, null, null), bin(200, 3, null)];
		const p = markerPoints(bins, 100, x, y, undefined, 4);
		expect(p).toEqual([
			{ x: 50, y: 95 },
			{ x: 250, y: 97 }
		]);
	});
	it('間引き: max(size*2.5, 8) px 未満は飛ばす', () => {
		const bins = Array.from({ length: 10 }, (_, i) => bin(i * 2, 5));
		const p = markerPoints(bins, 2, x, y, undefined, 2);
		expect(p.map((q) => q.x)).toEqual([1, 9, 17]);
		expect(markerPoints(bins, 2, x, y, undefined, 8).map((q) => q.x)).toEqual([1]);
	});
	it('範囲外は飛ばし、端の区間の x は範囲で切った中央', () => {
		const bins = [bin(0, 1), bin(100, 2), bin(200, 3)];
		const p = markerPoints(bins, 100, x, y, { startMs: 50, endMs: 200 }, 4);
		expect(p).toEqual([
			{ x: 75, y: 99 },
			{ x: 150, y: 98 }
		]);
	});
	it('envelopePath と同じ x', () => {
		const bins = [bin(0, 5), bin(100, 7)];
		const opt = { startMs: 20, endMs: 150 };
		const d = envelopePath(bins, 100, x, y, opt);
		const p = markerPoints(bins, 100, x, y, opt, 2);
		expect(d).toBe('M60,95L125,93');
		expect(p.map((q) => q.x)).toEqual([60, 125]);
	});
});

describe('markerPath', () => {
	const pts = [{ x: 10, y: 20 }];
	it('none・点なしは空', () => {
		expect(markerPath(pts, 'none', 4)).toBe('');
		expect(markerPath([], 'circle', 4)).toBe('');
		expect(markerPath(pts, null, 4)).toBe('');
	});
	it('形', () => {
		expect(markerPath(pts, 'circle', 4)).toBe('M8,20a2,2 0 1,0 4,0a2,2 0 1,0 -4,0Z');
		expect(markerPath(pts, 'square', 4)).toBe('M8,18h4v4h-4Z');
		expect(markerPath(pts, 'triangle', 4)).toBe('M10,18L12,22L8,22Z');
		expect(markerPath(pts, 'diamond', 4)).toBe('M10,18L12,20L10,22L8,20Z');
	});
	it('複数の点を 1 本に', () => {
		const d = markerPath([...pts, { x: 30, y: 20 }], 'square', 4);
		expect(d).toBe('M8,18h4v4h-4ZM28,18h4v4h-4Z');
	});
});
