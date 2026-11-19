import { describe, expect, it } from 'vitest';
import { bandLayout, bandRects, type TrendBand } from './bands';
import type { TrendPen } from './pens';

const band = (from: number | null, to: number | null, label = 'B'): TrendBand => ({
	from,
	to,
	label,
	colorVar: 'var(--x)'
});

describe('bandRects', () => {
	// 縦軸 0〜100、高さ 200 px（値 1 = 2 px）。
	const domain: [number, number] = [0, 100];

	it('閉じた帯は値の範囲の位置に置く（下端が下、上端が上）', () => {
		expect(bandRects([band(20, 70)], domain, 200)).toEqual([
			{ y: 60, height: 100, colorVar: 'var(--x)', label: 'B' }
		]);
	});

	it('開いた帯は縦軸の端まで伸ばす', () => {
		expect(bandRects([band(80, null, 'HH')], domain, 200)).toEqual([
			{ y: 0, height: 40, colorVar: 'var(--x)', label: 'HH' }
		]);
		expect(bandRects([band(null, 10, 'LL')], domain, 200)).toEqual([
			{ y: 180, height: 20, colorVar: 'var(--x)', label: 'LL' }
		]);
		expect(bandRects([band(null, null)], domain, 200)).toEqual([
			{ y: 0, height: 200, colorVar: 'var(--x)', label: 'B' }
		]);
	});

	it('縦軸から一部はみ出す帯は切り取り、外の帯は出さない', () => {
		expect(bandRects([band(-50, 10)], domain, 200)).toEqual([
			{ y: 180, height: 20, colorVar: 'var(--x)', label: 'B' }
		]);
		expect(bandRects([band(120, null)], domain, 200)).toEqual([]);
		expect(bandRects([band(null, -1)], domain, 200)).toEqual([]);
	});

	it('幅 0（from == to・縦軸の端にちょうど接する）の帯は出さない', () => {
		expect(bandRects([band(50, 50)], domain, 200)).toEqual([]);
		expect(bandRects([band(100, null)], domain, 200)).toEqual([]);
		expect(bandRects([band(null, 0)], domain, 200)).toEqual([]);
	});

	it('from > to は入れ替え、非有限の数の帯は出さない', () => {
		expect(bandRects([band(70, 20)], domain, 200)).toEqual([
			{ y: 60, height: 100, colorVar: 'var(--x)', label: 'B' }
		]);
		expect(bandRects([band(Number.NaN, 20)], domain, 200)).toEqual([]);
		expect(bandRects([band(10, Infinity)], domain, 200)).toEqual([]);
	});

	it('描画域の高さが 0・縦軸が不正なら空', () => {
		expect(bandRects([band(20, 70)], domain, 0)).toEqual([]);
		expect(bandRects([band(20, 70)], [5, 5], 200)).toEqual([]);
	});
});

describe('bandLayout', () => {
	const pens: TrendPen[] = [
		{ id: 'a', label: 'A', colorSlot: 1 },
		{ id: 'b', label: 'B', colorSlot: 2 }
	];
	const domains: [number, number][] = [
		[0, 10],
		[0, 100]
	];

	it('指定したペンの縦軸で置く', () => {
		expect(bandLayout({ penId: 'b', items: [band(50, null)] }, pens, domains, 100)).toEqual([
			{ y: 0, height: 50, colorVar: 'var(--x)', label: 'B' }
		]);
		expect(bandLayout({ penId: 'a', items: [band(5, null)] }, pens, domains, 100)).toEqual([
			{ y: 0, height: 50, colorVar: 'var(--x)', label: 'B' }
		]);
	});

	it('ペンが居ない・bands が null なら空', () => {
		expect(bandLayout({ penId: 'z', items: [band(0, 1)] }, pens, domains, 100)).toEqual([]);
		expect(bandLayout(null, pens, domains, 100)).toEqual([]);
	});
});
