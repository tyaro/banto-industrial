import { describe, expect, it } from 'vitest';
import { envelopePath } from './geometry';
import type { Bin } from './pens';

// x: 時刻をそのまま px に、y: 値を反転（上が大きい）。
const x = (t: number) => t;
const y = (v: number) => 100 - v;

describe('envelopePath', () => {
	it('各区間は区間の中央の縦線で、前の点に近い端から入る', () => {
		const bins: Bin[] = [
			{ tMs: 0, min: 10, max: 20 },
			{ tMs: 10, min: 15, max: 30 },
			{ tMs: 20, min: 0, max: 18 }
		];
		// 1 区間目: min→max（y 90→80）。2 区間目: 前の y=80 に近いのは max(30→y70)? |70-80|=10,
		// min(15→y85) は |85-80|=5 なので min から入る。3 区間目: 前の y=70、max(18→82) が近い。
		expect(envelopePath(bins, 10, x, y)).toBe('M5,90L5,80L15,85L15,70L25,82L25,100');
	});

	it('値が 1 つの区間は点 1 つ。片方だけ有限も 1 つ', () => {
		const bins: Bin[] = [
			{ tMs: 0, min: 10, max: 10 },
			{ tMs: 10, min: null, max: 20 },
			{ tMs: 20, min: 30, max: null }
		];
		expect(envelopePath(bins, 10, x, y)).toBe('M5,90L15,80L25,70');
	});

	it('欠測・非有限で線を切る。1 点だけの切れ端は同じ点への L を足す', () => {
		const bins: Bin[] = [
			{ tMs: 0, min: 10, max: 10 },
			{ tMs: 10, min: null, max: null },
			{ tMs: 20, min: 1, max: 2 },
			{ tMs: 30, min: NaN, max: Infinity },
			{ tMs: 40, min: 5, max: 5 }
		];
		expect(envelopePath(bins, 10, x, y)).toBe('M5,90L5,90M25,99L25,98M45,95L45,95');
	});

	it('空なら空文字。座標を小数 2 桁に丸める', () => {
		expect(envelopePath([], 10, x, y)).toBe('');
		expect(envelopePath([{ tMs: 0, min: 1 / 3, max: 1 / 3 }], 1 / 3, x, y)).toBe(
			'M0.17,99.67L0.17,99.67'
		);
	});

	it('min > max の区間も縦線にする', () => {
		expect(envelopePath([{ tMs: 0, min: 20, max: 10 }], 10, x, y)).toBe('M5,90L5,80');
	});
});
