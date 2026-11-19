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
				thresholds: null
			},
			{
				tagId: 2,
				name: '運転',
				unit: null,
				decimals: null,
				isBit: true,
				colorSlot: 1,
				thresholds: null
			}
		]);
		expect(pens[0]).toEqual({
			id: trendPenId(0, 1),
			label: '炉温（℃）',
			colorSlot: 3,
			decimals: 1
		});
		expect(pens[1]).toMatchObject({ id: trendPenId(1, 2), yMin: 0, yMax: 1, decimals: null });
	});
});
