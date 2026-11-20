import { describe, expect, it } from 'vitest';
import { resolveYDomain, type Bin } from '@banto-industrial/trend';
import { trendPenInfos } from './trendLogic';
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
				engRange: null,
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
				engRange: null,
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
			engRange: null,
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

	describe('縦軸の範囲にタグの工学値レンジを使う', () => {
		const bins = (vs: number[]): Bin[] =>
			vs.map((v, i) => ({ tMs: i * 1000, min: v, max: v }) as unknown as Bin);
		const domainOf = (
			tag: Record<string, unknown>,
			pen: { yMin?: number | null; yMax?: number | null },
			data: number[]
		) => {
			const infos = trendPenInfos({ pens: [{ tagId: 1, colorSlot: null, ...pen }] }, [
				{
					id: 1,
					name: 'x',
					unit: '',
					decimals: 1,
					thresholdHh: null,
					thresholdH: null,
					thresholdL: null,
					thresholdLl: null,
					...tag
				}
			] as unknown as Parameters<typeof trendPenInfos>[1]);
			return resolveYDomain(toTrendPens(infos)[0], bins(data));
		};

		it('工学値レンジ 0〜100 で yMin=10 だけ指定、データ 20〜30 なら 10〜100', () => {
			expect(domainOf({ engLo: 0, engHi: 100 }, { yMin: 10 }, [20, 30])).toEqual([10, 100]);
		});

		it('両側とも指定しなければ 0〜100', () => {
			expect(domainOf({ engLo: 0, engHi: 100 }, {}, [20, 30])).toEqual([0, 100]);
		});

		it('工学値レンジが無い・逆順・同値・非有限ならデータの範囲', () => {
			expect(domainOf({ engLo: null, engHi: null }, {}, [20, 30])).toEqual([20, 30]);
			expect(domainOf({ engLo: 100, engHi: 0 }, {}, [20, 30])).toEqual([20, 30]);
			expect(domainOf({ engLo: 5, engHi: 5 }, {}, [20, 30])).toEqual([20, 30]);
			expect(domainOf({ engLo: 0, engHi: Number.NaN }, {}, [20, 30])).toEqual([20, 30]);
		});

		it('bit のタグは工学値レンジがあっても 0〜1 のまま', () => {
			const [pen] = toTrendPens([
				{
					tagId: 1,
					name: 'b',
					unit: null,
					decimals: null,
					isBit: true,
					colorSlot: 1,
					axis: null,
					yMin: null,
					yMax: null,
					engRange: { min: 0, max: 100 },
					thresholds: null
				}
			]);
			expect(pen).toMatchObject({ yMin: 0, yMax: 1 });
			expect('engRange' in pen).toBe(false);
			expect(resolveYDomain(pen, bins([0, 1]))).toEqual([0, 1]);
		});
	});
});
