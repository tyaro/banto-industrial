import { describe, expect, it } from 'vitest';
import { EMPTY_PEN_STYLE } from '#lib/banto/displayGroupsAdmin.js';
import {
	MAX_PEN_LABEL_CHARS,
	MAX_PEN_UNIT_CHARS,
	axisText,
	bitBandHeightOptions,
	penStyleProblems,
	styleOf
} from './penStyleEditor';

const pen = (patch: object = {}) => ({ ...EMPTY_PEN_STYLE, ...patch });

describe('styleOf', () => {
	it('名前・単位を整え、数の欄は入力の文字を読む（読めなければ null）', () => {
		const style = styleOf(
			pen({ label: ' A ', unit: ' ', yMin: 1, texts: { yMin: '2', yMax: 'x' } })
		);
		expect(style.label).toBe('A');
		expect(style.unit).toBeNull();
		expect(style.yMin).toBe(2);
		expect(style.yMax).toBeNull();
		expect(Object.keys(style)).not.toContain('texts');
	});
});

describe('axisText', () => {
	it('入力の文字があればそれ、無ければ値', () => {
		expect(axisText(pen({ yMin: 3 }), 'yMin')).toBe('3');
		expect(axisText(pen({ yMin: 3, texts: { yMin: '3.' } }), 'yMin')).toBe('3.');
		expect(axisText(pen(), 'limitH')).toBe('');
	});
});

describe('penStyleProblems', () => {
	it('誤りの無いペンは空', () => {
		expect(penStyleProblems(pen({ yMin: 0, yMax: 1, limitL: 1, limitH: 2 }), 1)).toEqual([]);
	});
	it('名前・単位の長さ（code point）', () => {
		expect(
			penStyleProblems(
				pen({ label: '😀'.repeat(MAX_PEN_LABEL_CHARS), unit: 'u'.repeat(MAX_PEN_UNIT_CHARS + 1) }),
				2
			)
		).toEqual([`ペン 2 の単位は ${MAX_PEN_UNIT_CHARS} 文字までです`]);
	});
	it('数として読めない文字・最小 >= 最大・管理線の順序', () => {
		expect(penStyleProblems(pen({ texts: { yMax: '-', limitLl: 'e' } }), 3)).toEqual([
			'ペン 3 の縦軸の最大が数ではありません',
			'ペン 3 の管理線の LL が数ではありません'
		]);
		expect(penStyleProblems(pen({ yMin: 5, texts: { yMax: '5' } }), 1)).toEqual([
			'ペン 1 の縦軸は、最小を最大より小さくしてください'
		]);
		expect(penStyleProblems(pen({ limitLl: 3, limitL: 2 }), 1)).toEqual([
			'ペン 1 の管理線は、LL ≦ L ≦ H ≦ HH の順にしてください（LL が L より大きい）'
		]);
	});
});

describe('bitBandHeightOptions', () => {
	it('並びに無い保存値も出す', () => {
		expect(bitBandHeightOptions(null)).toContain(16);
		expect(bitBandHeightOptions(10)).toEqual([8, 10, 12, 16, 20, 24, 28, 32]);
	});
});
