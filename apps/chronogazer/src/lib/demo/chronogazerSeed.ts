/**
 * ChronoGazer の模擬の初期のデータ（設計 docs/demo-pages-design.md §3.3）: 模擬の PLC の接続
 * 2 本、収集グループ 2 つ、タグ 20 本（数値・bit）、表示グループ 2 つ。アドレスは見た目だけ。
 *
 * ChronoGazer の `Tag` は文字列型（`string`）を持たず、収集グループに `defaultWritable` も無い
 * （Hub との違い）。表示グループのペンは次の PR の `/monitor` で使う。
 */
import { EMPTY_PEN_STYLE, type DisplayGroup, type Pen } from '#lib/banto/displayGroupsAdmin.js';
import type {
	CollectionGroup,
	PlcConnection,
	Tag,
	TagDataType
} from '#lib/banto/tagRegistryAdmin.js';

export function seedConnections(): PlcConnection[] {
	return [
		{
			id: 1,
			name: 'line1',
			protocol: 'slmp',
			enabled: true,
			simulation: true,
			settings: { host: '192.0.2.10', port: 5007, cpu_type: 'R', word_order: 'low_high' }
		},
		{
			id: 2,
			name: 'line2',
			protocol: 'modbus-tcp',
			enabled: true,
			simulation: true,
			settings: { host: '192.0.2.20', port: 502, unit_id: 1, word_order: 'high_low' }
		}
	];
}

export function seedGroups(): CollectionGroup[] {
	return [
		{ id: 1, name: 'press', plcConnectionId: 1, periodMs: 500, enabled: true },
		{ id: 2, name: 'pump', plcConnectionId: 2, periodMs: 1000, enabled: true }
	];
}

type SeedRow = [
	name: string,
	group: number,
	address: string,
	dataType: TagDataType,
	unit: string | null,
	decimals: number
];

const ROWS: SeedRow[] = [
	['press_pressure', 1, 'D100', 'f32', 'MPa', 2],
	['press_position', 1, 'D102', 'f32', 'mm', 1],
	['press_speed', 1, 'D104', 'f32', 'mm/s', 1],
	['press_count', 1, 'D110', 'u32', '個', 0],
	['press_temp', 1, 'D120', 'i16', '℃', 0],
	['press_load', 1, 'D122', 'u16', '%', 0],
	['press_running', 1, 'M100', 'bit', null, 0],
	['press_alarm', 1, 'M101', 'bit', null, 0],
	['press_door', 1, 'M102', 'bit', null, 0],
	['press_cycle', 1, 'D130', 'f32', 's', 2],
	['pump_flow', 2, '40001', 'u16', 'L/min', 0],
	['pump_pressure', 2, '40002', 'f32', 'kPa', 1],
	['pump_current', 2, '40004', 'f32', 'A', 1],
	['pump_temp', 2, '40006', 'i16', '℃', 0],
	['pump_rpm', 2, '40007', 'u16', 'rpm', 0],
	['pump_level', 2, '40008', 'u16', '%', 0],
	['pump_total', 2, '40010', 'u32', 'm3', 0],
	['pump_run', 2, '00001', 'bit', null, 0],
	['pump_fault', 2, '00002', 'bit', null, 0],
	['pump_vibration', 2, '40012', 'f32', 'mm/s', 2]
];

export function seedTags(): Tag[] {
	return ROWS.map(([name, group, address, dataType, unit, decimals], index) => ({
		id: index + 1,
		name,
		collectionGroupId: group,
		address,
		dataType,
		rawLo: null,
		rawHi: null,
		engLo: null,
		engHi: null,
		unit,
		decimals,
		enabled: true,
		revision: 1
	}));
}

function pens(...tagIds: number[]): Pen[] {
	return tagIds.map((tagId) => ({ ...EMPTY_PEN_STYLE, tagId, colorSlot: null }));
}

/** 表示グループ 2 つ（トレンドの束）。ペンは初期のタグの数本。 */
export function seedDisplayGroups(): DisplayGroup[] {
	return [
		{
			id: 1,
			name: 'プレス',
			bundle: null,
			sortOrder: 0,
			kind: 'trend',
			attributes: { timeWindowSec: 600 },
			pens: pens(1, 2, 3, 5),
			revision: 1
		},
		{
			id: 2,
			name: 'ポンプ',
			bundle: null,
			sortOrder: 1,
			kind: 'trend',
			attributes: { timeWindowSec: 600 },
			pens: pens(11, 12, 13, 14),
			revision: 1
		}
	];
}
