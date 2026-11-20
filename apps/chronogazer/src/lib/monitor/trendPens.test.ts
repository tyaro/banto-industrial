import { describe, expect, it } from 'vitest';
import { tagIdOfTrendPenId, toTrendPens, trendPenId } from './trendPens';

describe('ペンの ID と部品のペン', () => {
	it('並びとタグ ID から作り、同じタグが 2 回でも衝突しない', () => {
		expect(trendPenId(0, 7)).not.toBe(trendPenId(1, 7));
		expect(tagIdOfTrendPenId(trendPenId(3, 42))).toBe(42);
		expect(tagIdOfTrendPenId('pen-x')).toBeNull();
	});

	it('色の枠・名前・小数桁を写し、bit は縦軸 0〜1・小数桁なし', () => {
		const pens = toTrendPens([
			{
				tagId: 1,
				name: '炉温',
				unit: '℃',
				decimals: 1,
				isBit: false,
				colorSlot: 3,
				axis: null,
				yMin: null,
				yMax: null,
				thresholds: null
			},
			{
				tagId: 2,
				name: '運転',
				unit: null,
				decimals: null,
				isBit: true,
				colorSlot: 1,
				axis: null,
				yMin: null,
				yMax: null,
				thresholds: null
			}
		]);
		expect(pens[0]).toEqual({
			id: trendPenId(0, 1),
			label: '炉温（℃）',
			colorSlot: 3,
			unit: '℃',
			decimals: 1
		});
		expect(pens[1]).toMatchObject({ id: trendPenId(1, 2), yMin: 0, yMax: 1, decimals: null });
	});

	it('縦軸の側・範囲を渡す。bit は 0〜1 固定で、範囲・小数桁は無視し、軸の側だけ効く', () => {
		const base = {
			tagId: 1,
			name: 'x',
			unit: null,
			decimals: 2,
			colorSlot: 1,
			axis: 'right' as const,
			yMin: -5,
			yMax: 50,
			thresholds: null
		};
		const [num, bit, half, plain] = toTrendPens([
			{ ...base, isBit: false },
			{ ...base, isBit: true },
			{ ...base, isBit: false, yMax: null },
			{ ...base, isBit: false, axis: null, yMin: null, yMax: null }
		]);
		expect(num).toMatchObject({ axis: 'right', yMin: -5, yMax: 50, decimals: 2 });
		expect(bit).toMatchObject({ axis: 'right', yMin: 0, yMax: 1, decimals: null });
		expect(half.yMin).toBe(-5);
		expect('yMax' in half).toBe(false);
		expect('axis' in plain || 'yMin' in plain || 'yMax' in plain).toBe(false);
	});
});
