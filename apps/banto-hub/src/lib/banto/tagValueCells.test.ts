import { describe, expect, it } from 'vitest';
import { tagValueCell, tagValueSortKey, tagValueTime } from './tagValueCells';
import type { RowValue } from './monitorValues';

const values = new Map<string, RowValue>([
	['PLC.G.a', { v: 12.5, q: 'good', t: 1_700_000_000_000 }],
	['PLC.G.bad', { v: null, q: 'bad', t: 1_700_000_000_000 }],
	['PLC.G.new', { v: null, q: 'bad', t: 0, unverified: true }]
]);

describe('tagValueCell', () => {
	it('良好な値は単位付きで出す', () => {
		expect(tagValueCell(values, 'PLC.G.a', 'kPa', 'live')).toEqual({
			value: '12.5 kPa',
			qualityClass: 'good',
			qualityLabel: '良好'
		});
	});
	it('値を受けていないタグは陳腐化で `--`', () => {
		expect(tagValueCell(values, 'PLC.G.none', null, 'live')).toEqual({
			value: '--',
			qualityClass: 'stale',
			qualityLabel: '陳腐化'
		});
	});
	it('未確認は区別して出す', () => {
		expect(tagValueCell(values, 'PLC.G.new', null, 'live').qualityLabel).toBe('未確認');
	});
	it('再接続の直後（スナップショット待ち）は古い値を出さない', () => {
		expect(tagValueCell(values, 'PLC.G.a', 'kPa', 'awaitingSnapshot')).toEqual({
			value: '--',
			qualityClass: 'stale',
			qualityLabel: '陳腐化'
		});
	});
	it('止まっている間は最終受信値を陳腐化として出す', () => {
		const cell = tagValueCell(values, 'PLC.G.a', null, 'halted');
		expect(cell.value).toBe('12.5');
		expect(cell.qualityLabel).toBe('陳腐化（受信時: 良好）');
	});
});

describe('tagValueTime', () => {
	it('受信前は `--`', () => {
		expect(tagValueTime(values, 'PLC.G.none')).toBe('--');
		expect(tagValueTime(values, 'PLC.G.new')).toBe('--');
	});
	it('受信後は日時の文字列', () => {
		expect(tagValueTime(values, 'PLC.G.a')).toBe(
			new Date(1_700_000_000_000).toLocaleString('ja-JP')
		);
	});
});

describe('tagValueSortKey', () => {
	it('良好な値だけ数値、それ以外は null', () => {
		expect(tagValueSortKey(values, 'PLC.G.a')).toBe(12.5);
		expect(tagValueSortKey(values, 'PLC.G.bad')).toBeNull();
		expect(tagValueSortKey(values, 'PLC.G.none')).toBeNull();
	});
});
