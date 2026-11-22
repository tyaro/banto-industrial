import { describe, expect, it } from 'vitest';
import { demoSampleTime, demoTagValue, demoValueIsNumeric, effectivePeriodMs } from './tagValues';

describe('demoTagValue', () => {
	it('同じ入力なら同じ値（純関数）', () => {
		const tag = { id: 12, dataType: 'f32', decimals: 2 };
		expect(demoTagValue(tag, 1_700_000_000_000)).toBe(demoTagValue(tag, 1_700_000_000_000));
	});

	it('更新の周期の間は同じ値で、時間が経つと変わる', () => {
		const tag = { id: 3, dataType: 'f64', decimals: 3, periodMs: 1000 };
		const t0 = 1_700_000_000_000;
		expect(demoTagValue(tag, t0)).toBe(demoTagValue(tag, t0 + 400));
		const values = new Set<number | null>();
		for (let i = 0; i < 20; i++) values.add(demoTagValue(tag, t0 + i * 5000));
		expect(values.size).toBeGreaterThan(5);
	});

	it('周期の下限は 500ms', () => {
		expect(effectivePeriodMs(100)).toBe(500);
		expect(effectivePeriodMs(undefined)).toBe(1000);
		expect(demoSampleTime(1_000_700, 100)).toBe(1_000_500);
	});

	it('整数の型は整数、小数の型は桁数に収まる', () => {
		for (let i = 0; i < 50; i++) {
			expect(Number.isInteger(demoTagValue({ id: 5, dataType: 'i16' }, i * 7919)!)).toBe(true);
		}
		const f = demoTagValue({ id: 9, dataType: 'f32', decimals: 1 }, 123456)!;
		expect(Number(f.toFixed(1))).toBe(f);
	});

	it('bit は 0 と 1 を切り替える', () => {
		const seen = new Set<number | null>();
		for (let i = 0; i < 40; i++) seen.add(demoTagValue({ id: 4, dataType: 'bit' }, i * 1000));
		expect([...seen].sort()).toEqual([0, 1]);
	});

	it('文字列は数値の値を持たない', () => {
		expect(demoValueIsNumeric('string')).toBe(false);
		expect(demoTagValue({ id: 1, dataType: 'string' }, 0)).toBeNull();
	});

	it('値は有限で、極端に外れない', () => {
		for (let id = 1; id <= 30; id++) {
			for (let i = 0; i < 20; i++) {
				const v = demoTagValue({ id, dataType: 'u16' }, i * 30_000)!;
				expect(v).toBeGreaterThanOrEqual(0);
				expect(v).toBeLessThan(200);
			}
		}
	});
});
