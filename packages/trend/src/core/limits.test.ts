import { describe, expect, it } from 'vitest';
import { limitLayout, type TrendLimit } from './limits';

const up = (value: number, label = 'H', colorVar = 'c1'): TrendLimit => ({
	value,
	side: 'upper',
	colorVar,
	label
});
const low = (value: number, label = 'L', colorVar = 'c2'): TrendLimit => ({
	value,
	side: 'lower',
	colorVar,
	label
});
// 範囲 0〜100、高さ 200 → 値 v の y = 200 - 2v
const D: [number, number] = [0, 100];

describe('limitLayout', () => {
	it('範囲の内の upper: 線と、上端から線までの矩形', () => {
		const r = limitLayout([up(80)], D, 200);
		expect(r.lines).toEqual([{ y: 40, colorVar: 'c1', label: 'H' }]);
		expect(r.rects).toEqual([{ y: 0, height: 40, colorVar: 'c1' }]);
	});
	it('範囲の内の lower: 線と、線から下端までの矩形', () => {
		const r = limitLayout([low(10)], D, 200);
		expect(r.lines[0].y).toBe(180);
		expect(r.rects).toEqual([{ y: 180, height: 20, colorVar: 'c2' }]);
	});
	it('upper が縦軸より上: 線も矩形も無い。下なら全部が越えている', () => {
		expect(limitLayout([up(150)], D, 200)).toEqual({ lines: [], rects: [] });
		const r = limitLayout([up(-5)], D, 200);
		expect(r.lines).toEqual([]);
		expect(r.rects).toEqual([{ y: 0, height: 200, colorVar: 'c1' }]);
	});
	it('lower が縦軸より上: 全部が越えている。下なら無い', () => {
		const r = limitLayout([low(150)], D, 200);
		expect(r.lines).toEqual([]);
		expect(r.rects).toEqual([{ y: 0, height: 200, colorVar: 'c2' }]);
		expect(limitLayout([low(-5)], D, 200)).toEqual({ lines: [], rects: [] });
	});
	it('縦軸の端ちょうどの線は引く。矩形は高さ 0 なので出さない', () => {
		const r = limitLayout([up(100), low(0)], D, 200);
		expect(r.lines.map((l) => l.y)).toEqual([0, 200]);
		expect(r.rects).toEqual([]);
	});
	it('非有限の値は捨てる', () => {
		const r = limitLayout([up(NaN), low(Infinity), up(80)], D, 200);
		expect(r.lines).toHaveLength(1);
		expect(r.rects).toHaveLength(1);
	});
	it('空・null・undefined は空', () => {
		const empty = { lines: [], rects: [] };
		expect(limitLayout([], D, 200)).toEqual(empty);
		expect(limitLayout(null, D, 200)).toEqual(empty);
		expect(limitLayout(undefined, D, 200)).toEqual(empty);
	});
	it('並びの順を保つ', () => {
		const r = limitLayout([up(80, 'H', 'a'), up(90, 'HH', 'b'), low(10, 'L', 'c')], D, 200);
		expect(r.lines.map((l) => l.label)).toEqual(['H', 'HH', 'L']);
		expect(r.rects.map((x) => x.colorVar)).toEqual(['a', 'b', 'c']);
	});
	it('plotHeight 0・縦軸が不正なら空', () => {
		const empty = { lines: [], rects: [] };
		expect(limitLayout([up(50)], D, 0)).toEqual(empty);
		expect(limitLayout([up(50)], [5, 5], 200)).toEqual(empty);
		expect(limitLayout([up(50)], [NaN, 5], 200)).toEqual(empty);
	});
});
