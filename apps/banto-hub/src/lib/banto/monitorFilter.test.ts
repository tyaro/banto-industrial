/**
 * `monitorFilter.ts`（T18-4a）に対するユニットテスト。`tagFormNumeric.test.ts`
 * と同じスタイル（describe/it、依存ゼロの純関数を直接 import）。
 */
import { describe, expect, it } from 'vitest';
import { filterMonitorRows, type MonitorFilterableRow } from './monitorFilter';

function row(
	ids: [number, number, number],
	overrides: Partial<MonitorFilterableRow> = {}
): MonitorFilterableRow {
	return {
		ids,
		external_name: `conn${ids[0]}.group${ids[1]}.tag${ids[2]}`,
		name: `tag${ids[2]}`,
		address: `D${ids[2]}00`,
		...overrides
	};
}

describe('filterMonitorRows', () => {
	const rows: MonitorFilterableRow[] = [
		row([1, 10, 100], {
			external_name: 'plc1.groupA.Temperature',
			name: 'Temperature',
			address: 'D100'
		}),
		row([1, 11, 101], { external_name: 'plc1.groupB.Pressure', name: 'Pressure', address: 'D200' }),
		row([2, 20, 200], { external_name: 'plc2.groupC.Flow', name: 'Flow', address: 'D300' })
	];

	it('all: 全件を素通しする', () => {
		expect(filterMonitorRows(rows, { type: 'all' }, '')).toEqual(rows);
	});

	it('connection: ids[0] が一致する行だけ残す', () => {
		const result = filterMonitorRows(rows, { type: 'connection', id: 1 }, '');
		expect(result.map((r) => r.name)).toEqual(['Temperature', 'Pressure']);
	});

	it('connection: 一致する行が無ければ空配列', () => {
		expect(filterMonitorRows(rows, { type: 'connection', id: 999 }, '')).toEqual([]);
	});

	it('group: ids[1] が一致する行だけ残す', () => {
		const result = filterMonitorRows(rows, { type: 'group', id: 11 }, '');
		expect(result.map((r) => r.name)).toEqual(['Pressure']);
	});

	it('search: 空クエリは素通しする', () => {
		expect(filterMonitorRows(rows, { type: 'all' }, '   ')).toEqual(rows);
	});

	it('search: external_name の部分一致でヒットする', () => {
		const result = filterMonitorRows(rows, { type: 'all' }, 'groupA');
		expect(result.map((r) => r.name)).toEqual(['Temperature']);
	});

	it('search: name の部分一致でヒットする', () => {
		const result = filterMonitorRows(rows, { type: 'all' }, 'flow');
		expect(result.map((r) => r.name)).toEqual(['Flow']);
	});

	it('search: address の部分一致でヒットする', () => {
		const result = filterMonitorRows(rows, { type: 'all' }, 'd200');
		expect(result.map((r) => r.name)).toEqual(['Pressure']);
	});

	it('search: 大小文字を無視する', () => {
		const result = filterMonitorRows(rows, { type: 'all' }, 'TEMPERATURE');
		expect(result.map((r) => r.name)).toEqual(['Temperature']);
	});

	it('search: どのフィールドにも一致しなければ空配列', () => {
		expect(filterMonitorRows(rows, { type: 'all' }, 'nonexistent')).toEqual([]);
	});

	it('複合: ツリー選択と検索の両方を満たす行だけ残す', () => {
		const result = filterMonitorRows(rows, { type: 'connection', id: 1 }, 'pressure');
		expect(result.map((r) => r.name)).toEqual(['Pressure']);
	});

	it('複合: ツリー選択には合致するが検索に合致しない場合は空配列', () => {
		expect(filterMonitorRows(rows, { type: 'connection', id: 1 }, 'flow')).toEqual([]);
	});
});
