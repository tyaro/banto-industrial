import { describe, expect, it } from 'vitest';
import { binCount, binMs, MAX_BINS } from './bins';

describe('binCount', () => {
	it('幅 ÷ 1 前後で、1〜MAX_BINS', () => {
		expect(binCount(800)).toBe(800);
		expect(binCount(799.6)).toBe(800);
		expect(binCount(5000)).toBe(MAX_BINS);
		expect(binCount(0)).toBe(1);
		expect(binCount(-3)).toBe(1);
		expect(binCount(NaN)).toBe(1);
	});
});

describe('binMs', () => {
	it('範囲を覆う整数のミリ秒（切り上げ）', () => {
		expect(binMs({ startMs: 0, endMs: 3_600_000 }, 800)).toBe(4500);
		expect(binMs({ startMs: 0, endMs: 1000 }, 3)).toBe(334);
		expect(binMs({ startMs: 0, endMs: 10 }, 2000)).toBe(1);
	});
	it('不正な範囲は 1、不正な bins は 1 区間', () => {
		expect(binMs({ startMs: 1, endMs: 0 }, 10)).toBe(1);
		expect(binMs({ startMs: 0, endMs: 1000 }, NaN)).toBe(1000);
		expect(binMs({ startMs: 0, endMs: 1000 }, 0)).toBe(1000);
	});
});
