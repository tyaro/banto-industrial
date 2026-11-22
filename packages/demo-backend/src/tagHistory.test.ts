import { describe, expect, it } from 'vitest';
import { demoTagHistory, validateDemoHistoryRequest, type DemoHistoryTag } from './tagHistory';

const NOW = 1_700_000_000_000;
const NUM: DemoHistoryTag = {
	id: 5,
	dataType: 'f32',
	decimals: 2,
	periodMs: 1000,
	collected: true
};

describe('demoTagHistory', () => {
	it('点の数は bins 以下で、区間の始まりの時刻が昇順に並ぶ', () => {
		const { binMs, points } = demoTagHistory(NUM, NOW - 3_600_000, NOW, 300, NOW);
		expect(binMs).toBe(Math.ceil(3_600_001 / 300));
		expect(points.length).toBeGreaterThan(0);
		expect(points.length).toBeLessThanOrEqual(300);
		for (let i = 1; i < points.length; i++)
			expect(points[i]!.tMs).toBeGreaterThan(points[i - 1]!.tMs);
		expect(points[0]!.tMs).toBe(NOW - 3_600_000);
	});

	it('min <= max で、数値のタグは null にならない', () => {
		const { points } = demoTagHistory(NUM, NOW - 600_000, NOW, 120, NOW);
		for (const p of points) {
			expect(p.min).not.toBeNull();
			expect(p.min!).toBeLessThanOrEqual(p.max!);
		}
	});

	it('同じ入力なら同じ結果', () => {
		const a = demoTagHistory(NUM, NOW - 60_000, NOW, 30, NOW);
		const b = demoTagHistory(NUM, NOW - 60_000, NOW, 30, NOW);
		expect(a).toEqual(b);
	});

	it('未来の区間は点を出さず、now をまたぐ区間は now までを標本する', () => {
		const { points } = demoTagHistory(NUM, NOW - 30_000, NOW + 30_000, 60, NOW);
		expect(points.length).toBeLessThan(60);
		expect(points.at(-1)!.tMs).toBeLessThanOrEqual(NOW);
		const allFuture = demoTagHistory(NUM, NOW + 1000, NOW + 61_000, 10, NOW);
		expect(allFuture.points).toEqual([]);
	});

	it('bit は 0・1 の範囲、文字列と収集の対象外は欠測（null）', () => {
		const bit = demoTagHistory({ ...NUM, dataType: 'bit' }, NOW - 60_000, NOW, 30, NOW);
		for (const p of bit.points) {
			expect(p.min!).toBeGreaterThanOrEqual(0);
			expect(p.max!).toBeLessThanOrEqual(1);
		}
		const str = demoTagHistory({ ...NUM, dataType: 'string' }, NOW - 10_000, NOW, 5, NOW);
		expect(str.points.every((p) => p.min === null && p.max === null)).toBe(true);
		const off = demoTagHistory({ ...NUM, collected: false }, NOW - 10_000, NOW, 5, NOW);
		expect(off.points.length).toBeGreaterThan(0);
		expect(off.points.every((p) => p.min === null && p.max === null)).toBe(true);
	});

	it('1 区間 1 ミリ秒でも壊れない（bins が幅より多い）', () => {
		const { binMs, points } = demoTagHistory(NUM, NOW - 4, NOW, 100, NOW);
		expect(binMs).toBe(1);
		expect(points).toHaveLength(5);
	});
});

describe('validateDemoHistoryRequest', () => {
	const ok = (ids: number[], from = 0, to = 1000, bins = 10) =>
		validateDemoHistoryRequest(ids, from, to, bins).map((e) => e.field);

	it('正しい要求は誤りなし（31 日ちょうど・bins 2000 × 16 本は境界で通る）', () => {
		expect(ok([1])).toEqual([]);
		expect(ok([1], 0, 31 * 86_400_000, 2000)).toEqual([]);
		expect(
			ok(
				Array.from({ length: 16 }, (_, i) => i),
				0,
				1000,
				2000
			)
		).toEqual([]);
	});

	it('タグの本数・期間・bins・本数 × bins の上限', () => {
		expect(ok([])).toEqual(['tagIds']);
		expect(ok(Array.from({ length: 17 }, (_, i) => i))).toEqual(['tagIds']);
		expect(ok([1, 1, 1])).toEqual([]);
		expect(ok([1], 1000, 1000)).toEqual(['toMs']);
		expect(ok([1], 1000, 0)).toEqual(['toMs']);
		expect(ok([1], 0, 31 * 86_400_000 + 1)).toEqual(['toMs']);
		expect(ok([1], 0, 1000, 0)).toEqual(['bins']);
		expect(ok([1], 0, 1000, 2001)).toEqual(['bins']);
		expect(ok([1], 0, 1000, Number.NaN)).toEqual(['bins']);
		// 本数 × bins ≤ 32000 は、16 本 × 2000 ちょうどが上限で、他の上限が先に当たるので単独では外れない。
		expect(ok([1, 2, 3], 0, 1000, 2000)).toEqual([]);
	});
});
