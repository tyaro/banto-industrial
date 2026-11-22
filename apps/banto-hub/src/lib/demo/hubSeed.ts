/**
 * Hub の模擬の初期のデータ（設計 docs/demo-pages-design.md §3.3）: 模擬の PLC の接続 2 本、
 * 収集グループ 2 つ、タグ 20 本（数値・bit・文字列）。アドレスは見た目だけ。
 */
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
		{
			id: 1,
			name: 'press',
			plcConnectionId: 1,
			periodMs: 500,
			enabled: true,
			defaultWritable: false,
			querySql: null
		},
		{
			id: 2,
			name: 'pump',
			plcConnectionId: 2,
			periodMs: 1000,
			enabled: true,
			defaultWritable: false,
			querySql: null
		}
	];
}

type SeedRow = [
	name: string,
	comment: string,
	group: number,
	address: string,
	dataType: TagDataType,
	unit: string | null,
	decimals: number
];

const ROWS: SeedRow[] = [
	['press_pressure', 'プレス圧力', 1, 'D100', 'f32', 'MPa', 2],
	['press_position', 'ラム位置', 1, 'D102', 'f32', 'mm', 1],
	['press_speed', 'ラム速度', 1, 'D104', 'f32', 'mm/s', 1],
	['press_count', '生産数', 1, 'D110', 'u32', '個', 0],
	['press_temp', '金型温度', 1, 'D120', 'i16', '℃', 0],
	['press_running', '運転中', 1, 'M100', 'bit', null, 0],
	['press_alarm', '異常', 1, 'M101', 'bit', null, 0],
	['press_door', 'ドア閉', 1, 'M102', 'bit', null, 0],
	['press_mode', '運転モード', 1, 'D130', 'string', null, 0],
	['press_recipe', '品番', 1, 'D140', 'string', null, 0],
	['pump_flow', '流量', 2, '40001', 'u16', 'L/min', 0],
	['pump_pressure', '吐出圧', 2, '40002', 'f32', 'kPa', 1],
	['pump_current', '電流', 2, '40004', 'f32', 'A', 1],
	['pump_temp', '軸受温度', 2, '40006', 'i16', '℃', 0],
	['pump_rpm', '回転数', 2, '40007', 'u16', 'rpm', 0],
	['pump_level', 'タンク水位', 2, '40008', 'u16', '%', 0],
	['pump_total', '積算流量', 2, '40010', 'u32', 'm3', 0],
	['pump_run', '運転', 2, '00001', 'bit', null, 0],
	['pump_fault', '故障', 2, '00002', 'bit', null, 0],
	['pump_name', 'ポンプ名', 2, '40020', 'string', null, 0]
];

export function seedTags(): Tag[] {
	return ROWS.map(([name, comment, group, address, dataType, unit, decimals], index) => ({
		id: index + 1,
		name,
		comment,
		collectionGroupId: group,
		address,
		dataType,
		stringLength: dataType === 'string' ? 8 : null,
		stringEncoding: 'utf8',
		rawLo: null,
		rawHi: null,
		engLo: null,
		engHi: null,
		unit,
		decimals,
		enabled: true,
		writable: false,
		tagKind: 'plc',
		expression: null,
		retain: false,
		revision: 1
	}));
}
