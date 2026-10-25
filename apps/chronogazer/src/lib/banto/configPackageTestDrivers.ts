/**
 * 設定パッケージのテスト（`configPackage.test.ts`・`configPackageAdmin.test.ts`）が使う
 * `GET /api/drivers` の偽の応答（R2b-6b-3）。欄・必須・既定値は `banto-driver-modbus` /
 * `banto-driver-slmp` の `settings_schema`（各クレートの `src/base.rs`）と同じ:
 * Modbus は `host`（必須・既定なし）・`port`（必須・既定 502）・`unit_id`（任意・既定 1）・
 * `word_order`（任意・既定 `high_low`）、SLMP は `host`・`port`（既定 5007）・`cpu_type`
 * （既定 `R`）・`word_order`（既定 `low_high`）。秘密の欄と既定の無い任意の欄を確かめる
 * 偽のドライバ `fake-secret` も置く。アプリのコードからは読まない。
 */
import type { DriverInfo, DriverSettingField } from './tagRegistryAdmin';

function field(
	key: string,
	kind: DriverSettingField['kind'],
	options: { required?: boolean; default?: string | number | boolean } = {}
): DriverSettingField {
	return {
		key,
		label: key,
		help: null,
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

function driver(id: string, fields: DriverSettingField[]): DriverInfo {
	return {
		id,
		displayName: id,
		vendor: null,
		version: '0.3.0',
		capabilities: { read: true, write: true },
		transports: ['tcp'],
		settingsSchema: { fields },
		supportedKinds: ['u16'],
		addressHelp: '',
		kind: 'frame'
	};
}

export const MODBUS_DRIVER: DriverInfo = driver('modbus-tcp', [
	field('host', { type: 'host' }, { required: true }),
	field('port', { type: 'integer', min: 1, max: 65535 }, { required: true, default: 502 }),
	field('unit_id', { type: 'integer', min: 0, max: 255 }, { default: 1 }),
	field('word_order', { type: 'choice', options: WORD_ORDER_OPTIONS }, { default: 'high_low' })
]);

export const SLMP_DRIVER: DriverInfo = driver('slmp', [
	field('host', { type: 'host' }, { required: true }),
	field('port', { type: 'integer', min: 1, max: 65535 }, { required: true, default: 5007 }),
	field(
		'cpu_type',
		{
			type: 'choice',
			options: [
				{ value: 'R', label: 'iQ-R（R）' },
				{ value: 'Q', label: 'Qシリーズ（Q）' }
			]
		},
		{ default: 'R' }
	),
	field('word_order', { type: 'choice', options: WORD_ORDER_OPTIONS }, { default: 'low_high' })
]);

/** 秘密の欄（必須）と、既定値の無い任意の欄（`note`）を持つ偽のドライバ。 */
export const FAKE_SECRET_DRIVER: DriverInfo = driver('fake-secret', [
	field('host', { type: 'host' }, { required: true }),
	field('token', { type: 'secret' }, { required: true }),
	field('note', { type: 'text', max_len: 32, placeholder: null })
]);

export const TEST_DRIVERS: DriverInfo[] = [MODBUS_DRIVER, SLMP_DRIVER, FAKE_SECRET_DRIVER];
