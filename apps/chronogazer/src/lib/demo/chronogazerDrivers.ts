/**
 * `GET /api/drivers` の模擬（設計 docs/demo-pages-design.md §3.3）。ChronoGazer の接続の作成
 * フォームが出すドライバ（SLMP・Modbus/TCP）を返す。Hub 経由の接続（`protocol: 'hub'`）はドライバの
 * 一覧には出ない（「Hub から追加」が暗黙に作る）。欄・必須・既定値は `banto-driver-slmp`・
 * `banto-driver-modbus` の `settings_schema` と同じ。アドレスは見た目だけで、どこにもつながない。
 */
import type { DriverInfo, DriverSettingField } from '#lib/banto/tagRegistryAdmin.js';

function field(
	key: string,
	label: string,
	kind: DriverSettingField['kind'],
	options: { required?: boolean; default?: string | number | boolean; help?: string } = {}
): DriverSettingField {
	return {
		key,
		label,
		help: options.help ?? null,
		kind,
		required: options.required ?? false,
		default: options.default ?? null,
		group: 'basic'
	};
}

const WORD_ORDER_OPTIONS = [
	{ value: 'low_high', label: 'low_high（MELSEC標準）' },
	{ value: 'high_low', label: 'high_low（Modbus/IEEE慣習）' }
];

const WORD_KINDS = ['i16', 'u16', 'i32', 'u32', 'f32', 'string'];

export function demoDrivers(): DriverInfo[] {
	return [
		{
			id: 'slmp',
			displayName: 'SLMP（三菱電機 MELSEC）',
			vendor: '三菱電機',
			version: 'demo',
			capabilities: { read: true, write: true },
			transports: ['tcp'],
			settingsSchema: {
				fields: [
					field('host', 'ホスト', { type: 'host' }, { required: true }),
					field(
						'port',
						'ポート',
						{ type: 'integer', min: 1, max: 65535 },
						{ required: true, default: 5007 }
					),
					field(
						'cpu_type',
						'CPU の種類',
						{
							type: 'choice',
							options: [
								{ value: 'R', label: 'iQ-R（R）' },
								{ value: 'Q', label: 'Qシリーズ（Q）' }
							]
						},
						{ default: 'R' }
					),
					field(
						'word_order',
						'ワード順',
						{ type: 'choice', options: WORD_ORDER_OPTIONS },
						{ default: 'low_high' }
					)
				]
			},
			supportedKinds: ['bit', ...WORD_KINDS],
			addressHelp: 'D100（ワード）・M10（ビット）のように、デバイス名と番号で指定します。',
			kind: 'frame'
		},
		{
			id: 'modbus-tcp',
			displayName: 'Modbus/TCP',
			vendor: null,
			version: 'demo',
			capabilities: { read: true, write: true },
			transports: ['tcp'],
			settingsSchema: {
				fields: [
					field('host', 'ホスト', { type: 'host' }, { required: true }),
					field(
						'port',
						'ポート',
						{ type: 'integer', min: 1, max: 65535 },
						{ required: true, default: 502 }
					),
					field('unit_id', 'ユニット ID', { type: 'integer', min: 0, max: 255 }, { default: 1 }),
					field(
						'word_order',
						'ワード順',
						{ type: 'choice', options: WORD_ORDER_OPTIONS },
						{ default: 'high_low' }
					)
				]
			},
			supportedKinds: ['bit', ...WORD_KINDS, 'i64', 'u64', 'f64'],
			addressHelp: '40001（保持レジスタ）・00001（コイル）のように、5 桁の番号で指定します。',
			kind: 'frame'
		}
	];
}
